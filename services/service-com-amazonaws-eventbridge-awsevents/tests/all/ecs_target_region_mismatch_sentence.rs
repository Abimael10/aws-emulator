// SPDX-License-Identifier: CC0-1.0

//! The sentence `PutTargets` refuses an Amazon ECS target with when the cluster the
//! target names and the task definition its parameter block names live in two
//! Regions, driven over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: AWSEvents.PutTargets`) through the real `MultiServiceRouter`, so
//! routing, deserialization, the handler and serialization are all exercised.
//!
//! The rule: a refusal this service states as a whole sentence is sent as a whole
//! sentence — the fixed text with its variable parts read off the request, and the
//! sentence terminator the service writes after the last of them. For this class
//! that is `Region of ` + the target's `Arn` + ` does not match region of ` + the
//! block's `TaskDefinitionArn` + `.`, with HTTP 400 `ValidationException`, both ARNs
//! echoed exactly as the request wrote them and nothing of the request stored.
//!
//! The table spans the class along everything the sentence's variable parts can
//! differ in — which of the two ARNs names the foreign Region, the partition both
//! ARNs are written in, the account they name, the cluster name, and the task
//! definition's family and revision — plus the row the rule does NOT fire on, a pair
//! naming one Region, which is accepted and stored. So the sentence is read off the
//! request rather than off any one recorded pair.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "eu-west-2";

/// The event pattern every rule below carries.
const PATTERN: &str = r#"{"source":["com.demo.orders"]}"#;

/// The fixed parts of the sentence, between and around the two ARNs.
const OPENING: &str = "Region of ";
const JOINING: &str = " does not match region of ";
const TERMINATOR: char = '.';

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

/// The wire error code (`__type`), without its namespace.
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

/// The role EventBridge is handed to invoke an ECS cluster target (the delivery is
/// an `ecs:RunTask` on the cluster), which every row carries so that no row is
/// refused for the role before its parameters are read.
fn invoke_role() -> String {
    format!("arn:aws:iam::{}:role/events-invoke", account())
}

/// A rule on the default bus carrying [`PATTERN`], asserted to be created.
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

/// One row: the pair of ARNs a target submits, and whether the two name one Region.
struct Row {
    case: &'static str,
    cluster: String,
    task_definition: String,
    /// Whether the pair is one EventBridge accepts, i.e. names one Region.
    accepted: bool,
}

fn rows() -> Vec<Row> {
    let account = account();
    vec![
        Row {
            case: "a task definition of another Region than the cluster",
            cluster: format!("arn:aws:ecs:{REGION}:{account}:cluster/orders"),
            task_definition: format!("arn:aws:ecs:us-east-1:{account}:task-definition/orders:1"),
            accepted: false,
        },
        Row {
            // The reverse direction: the CLUSTER is the ARN of the foreign Region,
            // and it is still the one the sentence names first.
            case: "a cluster of another Region than the task definition",
            cluster: format!("arn:aws:ecs:ap-southeast-2:{account}:cluster/orders"),
            task_definition: format!("arn:aws:ecs:{REGION}:{account}:task-definition/orders:1"),
            accepted: false,
        },
        Row {
            // Neither ARN is of the Region the request is signed for, and the names
            // and revision differ from every other row.
            case: "a pair of Regions the request names neither of",
            cluster: format!("arn:aws:ecs:us-west-2:{account}:cluster/payments-prod"),
            task_definition: format!(
                "arn:aws:ecs:eu-central-1:{account}:task-definition/web-app-service:137"
            ),
            accepted: false,
        },
        Row {
            // Another partition: the ARN text is echoed whole, partition included.
            case: "two Regions of another partition",
            cluster: "arn:aws-cn:ecs:cn-north-1:111122223333:cluster/orders".to_string(),
            task_definition: "arn:aws-cn:ecs:cn-northwest-1:111122223333:task-definition/orders:9"
                .to_string(),
            accepted: false,
        },
        Row {
            // An account this caller has nothing in is refused for the account it
            // names, but only once its own configuration is installable: the pair of
            // Regions is judged first, so these ARNs are the ones echoed.
            case: "a pair in an account that is not the caller's",
            cluster: "arn:aws:ecs:us-east-2:444455556666:cluster/orders".to_string(),
            task_definition: "arn:aws:ecs:sa-east-1:444455556666:task-definition/orders:42"
                .to_string(),
            accepted: false,
        },
        Row {
            case: "a task definition of the cluster's own Region",
            cluster: format!("arn:aws:ecs:{REGION}:{account}:cluster/orders"),
            task_definition: format!("arn:aws:ecs:{REGION}:{account}:task-definition/orders:1"),
            accepted: true,
        },
        Row {
            // One Region, written in neither the signing Region nor the previous
            // row's: the rule is about the two ARNs agreeing, not about either one
            // matching the request.
            case: "a pair naming one Region, which is not the request's",
            cluster: format!("arn:aws:ecs:ca-central-1:{account}:cluster/orders-eu"),
            task_definition: format!(
                "arn:aws:ecs:ca-central-1:{account}:task-definition/orders-eu:3"
            ),
            accepted: true,
        },
    ]
}

#[tokio::test]
async fn a_cross_region_ecs_target_is_refused_with_the_whole_sentence() {
    let router = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let rule = format!("ecs-region-sentence-{index}");
        given_rule(&router, case, &rule).await;

        let target_id = format!("target-{index}");
        let (status, body) = call(
            &router,
            "PutTargets",
            &json!({
                "Rule": rule,
                "Targets": [{
                    "Id": target_id,
                    "Arn": row.cluster,
                    "RoleArn": invoke_role(),
                    "EcsParameters": { "TaskDefinitionArn": row.task_definition },
                }],
            }),
        )
        .await;

        if row.accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the target to be accepted, got {status} {body}"
            );
            let stored = target_ids(&router, case, &rule).await;
            assert_eq!(
                stored,
                vec![target_id.clone()],
                "case {case:?}: expected the rule to hold {target_id:?}, got {stored:?}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: expected the target to be refused, got {status} {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected ValidationException, got {code:?} in {body}"
        );

        let expected = format!(
            "{OPENING}{}{JOINING}{}{TERMINATOR}",
            row.cluster, row.task_definition
        );
        let actual = detail(&body);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );

        // A caller that reads the pair back out of the sentence — strip the
        // terminator, split on the fixed joining text — recovers the two ARNs it
        // submitted, in that order.
        let body_text = actual.strip_suffix(TERMINATOR).unwrap_or_else(|| {
            panic!("case {case:?}: expected a sentence ending in {TERMINATOR:?}, got {actual:?}")
        });
        let read: Vec<&str> = body_text
            .strip_prefix(OPENING)
            .unwrap_or_default()
            .split(JOINING)
            .collect();
        assert_eq!(
            read,
            vec![row.cluster.as_str(), row.task_definition.as_str()],
            "case {case:?}: expected the sentence to carry the submitted pair, got {read:?}"
        );

        let stored = target_ids(&router, case, &rule).await;
        assert!(
            stored.is_empty(),
            "case {case:?}: expected the rule to hold no target, got {stored:?}"
        );
    }
}
