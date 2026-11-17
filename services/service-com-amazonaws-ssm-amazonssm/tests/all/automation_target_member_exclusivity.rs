// SPDX-License-Identifier: CC0-1.0

//! The refusal of an automation input set that names its targets BOTH ways at once:
//! `Targets` and `TargetMaps` together, through each operation that takes automation
//! execution inputs — `StartAutomationExecution` and `StartExecutionPreview`.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol").
//!
//! The suite is a table. Each row is one shape of the two members — how many values a
//! target carries, which key form it uses, how many maps there are (including the largest
//! count the model admits) and how many keys each map carries — because the refusal is of
//! the two members being present TOGETHER, whatever either of them holds. A client that
//! branches on the refusal text to tell this mistake apart from the other invalid-input
//! refusals these starts raise reads one fixed sentence, so the sentence is asserted whole
//! rather than by a substring.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request signs for. Deliberately not the emulator's default, so a
/// refusal served here proves the Region came from the request's SigV4 credential scope.
const REGION: &str = "us-west-2";

/// The `X-Amz-Target` prefix from the crate's `OPERATIONS.md` wire-protocol section.
const TARGET_PREFIX: &str = "AmazonSSM";

/// The Automation runbook every case starts or previews.
const RUNBOOK: &str = "Exclusivity";

/// The runbook parameter a rate-controlled start binds its targets to. Declared WITH a
/// default, so a start that names no targets at all needs to supply nothing.
const TARGET_PARAMETER: &str = "Node";

/// An EC2 node id with the 8-character suffix form the modelled `InstanceId` pattern
/// admits.
const EC2_ONE: &str = "i-0abc1234";

/// A second EC2 node id, in the 17-character suffix form the same pattern admits.
const EC2_TWO: &str = "i-0123456789abcdef0";

/// The sentence Systems Manager refuses an automation input set that supplies both
/// `Targets` and `TargetMaps` with: FIXED text, naming the two members and nothing the
/// caller sent, terminated with a period.
const BOTH_TARGET_MEMBERS: &str = "Targets and TargetMaps cannot both be provided.";

/// The largest number of target maps the model admits (`TargetMaps`, @length max 300 — from
/// smithy_query(query="shape",shape="TargetMaps")), so the table has a row at the published
/// bound as well as the small counts a client usually sends.
const MAX_TARGET_MAPS: usize = 300;

/// A router with just this service mounted, and the engine it was mounted on — the handle a
/// case reads the store through and moves the clock with.
fn service() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what this test is about; the router still reads the SigV4
    // credential scope below, which is where the Region comes from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// Call `operation` signed for [`REGION`], returning `(status, body)`.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/ssm/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    // The error shape travels in `__type`; fold in the header the framework also sets so
    // one accessor reads either.
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// The wire error code of a refusal.
fn error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered
        .rsplit(['#', '/'])
        .next()
        .unwrap_or(rendered)
        .to_string()
}

/// The refusal message of a failure body.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// A string member of a response body.
fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or("<unset>").to_string()
}

/// Create the Automation runbook every case names: one step that waits for no time, and one
/// declared parameter — with a default — for a rate-controlled start to bind targets to.
async fn create_runbook(router: &MultiServiceRouter) {
    let mut parameters = serde_json::Map::new();
    parameters.insert(
        TARGET_PARAMETER.to_string(),
        json!({"type": "String", "default": EC2_ONE}),
    );
    let content = json!({
        "schemaVersion": "0.3",
        "parameters": Value::Object(parameters),
        "mainSteps": [{
            "name": "only",
            "action": "aws:sleep",
            "inputs": {"Duration": "PT0S"},
            "isEnd": true,
        }],
    });
    let (status, created) = call(
        router,
        "CreateDocument",
        &json!({
            "Name": RUNBOOK,
            "DocumentType": "Automation",
            "Content": content.to_string(),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the runbook must be created first, got {status} with {created:?}"
    );
}

/// The automation execution inputs a case sends, as the members a start names them by.
struct Inputs {
    target_parameter_name: Option<&'static str>,
    targets: Option<Value>,
    target_maps: Option<Value>,
}

/// The same inputs as a `StartAutomationExecution` body: the members sit on the request
/// itself.
fn automation_start_body(inputs: &Inputs) -> Value {
    let mut body = json!({"DocumentName": RUNBOOK});
    let map = body.as_object_mut().expect("an object body");
    if let Some(name) = inputs.target_parameter_name {
        map.insert("TargetParameterName".to_string(), json!(name));
    }
    if let Some(targets) = &inputs.targets {
        map.insert("Targets".to_string(), targets.clone());
    }
    if let Some(target_maps) = &inputs.target_maps {
        map.insert("TargetMaps".to_string(), target_maps.clone());
    }
    body
}

/// The same inputs as a `StartExecutionPreview` body: the members sit under
/// `ExecutionInputs.Automation`.
fn preview_start_body(inputs: &Inputs) -> Value {
    let mut automation = serde_json::Map::new();
    if let Some(name) = inputs.target_parameter_name {
        automation.insert("TargetParameterName".to_string(), json!(name));
    }
    if let Some(targets) = &inputs.targets {
        automation.insert("Targets".to_string(), targets.clone());
    }
    if let Some(target_maps) = &inputs.target_maps {
        automation.insert("TargetMaps".to_string(), target_maps.clone());
    }
    json!({
        "DocumentName": RUNBOOK,
        "ExecutionInputs": {"Automation": Value::Object(automation)},
    })
}

/// One target map of `bindings`: each document parameter mapped to the target resources it
/// takes its value from.
fn one_target_map(bindings: &[(&str, &[&str])]) -> Value {
    let mut map = serde_json::Map::new();
    for (key, values) in bindings {
        map.insert((*key).to_string(), json!(values));
    }
    Value::Object(map)
}

/// `n` target maps, each binding the runbook's parameter to one node.
fn target_maps(n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|_| one_target_map(&[(TARGET_PARAMETER, &[EC2_ONE])]))
            .collect(),
    )
}

/// Register the nodes a target of node ids resolves to, the way a node's own registration
/// would: a start whose targets resolve no resource names nothing to run for.
fn seed_nodes(engine: &Arc<engine::InMemoryEngine>) {
    let at = SystemTime::now();
    for id in [EC2_ONE, EC2_TWO] {
        assert!(
            engine
                .state::<ComAmazonawsSsmAmazonssmState>()
                .lock()
                .insert_managed_node(
                    REGION,
                    ManagedNodeRecord::registering(id, RESOURCE_TYPE_EC2_INSTANCE, at)
                ),
            "a seeded node must carry the node id it is stored under"
        );
    }
}

/// How many automation executions and execution previews the store holds, to pin that a
/// refused start persists nothing.
fn stored(engine: &Arc<engine::InMemoryEngine>) -> (usize, usize) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let store = state.lock();
    (
        store.automation_executions.len(),
        store.execution_previews.len(),
    )
}

/// Let the clock reach the instant every preview of [`REGION`] is due to have been worked
/// out at, rather than sleeping for the span the service owes it.
fn work_out(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsSsmAmazonssmState>();
    let mut store = state.lock();
    let long_ago = SystemTime::now() - Duration::from_secs(3_600);
    for ((held, _), preview) in store.execution_previews.iter_mut() {
        if held == REGION {
            preview.ready_at = long_ago;
        }
    }
}

/// An input set that names its targets both ways at once is refused with AWS's one fixed
/// sentence, by both operations that take automation execution inputs and whatever the two
/// members hold — and nothing is persisted for it.
#[tokio::test]
async fn naming_targets_both_ways_is_refused_with_the_one_fixed_sentence() {
    let (router, engine) = service();
    create_runbook(&router).await;
    let held = stored(&engine);

    let rows = [
        (
            "one id target and one single-key map",
            Inputs {
                target_parameter_name: Some(TARGET_PARAMETER),
                targets: Some(json!([{"Key": "InstanceIds", "Values": [EC2_ONE]}])),
                target_maps: Some(target_maps(1)),
            },
        ),
        (
            "a target of several ids and three maps",
            Inputs {
                target_parameter_name: Some(TARGET_PARAMETER),
                targets: Some(json!([{"Key": "InstanceIds", "Values": [EC2_ONE, EC2_TWO]}])),
                target_maps: Some(target_maps(3)),
            },
        ),
        (
            "a tag-key target and a map of two keys",
            Inputs {
                target_parameter_name: Some(TARGET_PARAMETER),
                targets: Some(json!([{"Key": "tag:Env", "Values": ["prod"]}])),
                target_maps: Some(Value::Array(vec![one_target_map(&[
                    (TARGET_PARAMETER, &[EC2_ONE]),
                    ("Other", &[EC2_TWO]),
                ])])),
            },
        ),
        (
            "a map whose value list is empty",
            Inputs {
                target_parameter_name: Some(TARGET_PARAMETER),
                targets: Some(json!([{"Key": "InstanceIds", "Values": [EC2_ONE]}])),
                target_maps: Some(Value::Array(vec![one_target_map(&[(
                    TARGET_PARAMETER,
                    &[],
                )])])),
            },
        ),
        (
            // The two members being present together is refused before either is resolved,
            // so a target key an automation resolves nothing by is still this refusal.
            "a target key an automation does not resolve by",
            Inputs {
                target_parameter_name: Some(TARGET_PARAMETER),
                targets: Some(json!([{"Key": "NotAKey", "Values": [EC2_ONE]}])),
                target_maps: Some(target_maps(1)),
            },
        ),
        (
            "as many maps as the model admits",
            Inputs {
                target_parameter_name: Some(TARGET_PARAMETER),
                targets: Some(json!([{"Key": "InstanceIds", "Values": [EC2_ONE]}])),
                target_maps: Some(target_maps(MAX_TARGET_MAPS)),
            },
        ),
    ];

    for (case, inputs) in &rows {
        // Each operation words this refusal through its own modelled error shape, and the
        // SENTENCE is the same for both.
        for (operation, body, expected_code) in [
            (
                "StartAutomationExecution",
                automation_start_body(inputs),
                "InvalidAutomationExecutionParametersException",
            ),
            (
                "StartExecutionPreview",
                preview_start_body(inputs),
                "ValidationException",
            ),
        ] {
            let case = format!("{operation}: {case}");
            let (status, response) = call(&router, operation, &body).await;
            assert_eq!(
                (status, error_code(&response).as_str()),
                (400, expected_code),
                "case {case:?}: expected HTTP 400 {expected_code}, got {status} with {response}"
            );
            assert_eq!(
                error_message(&response),
                BOTH_TARGET_MEMBERS,
                "case {case:?}: expected the fixed refusal sentence, got {response}"
            );
            assert_eq!(
                stored(&engine),
                held,
                "case {case:?}: expected a refused start to persist nothing"
            );
        }
    }
}

/// The positive control of the same rule: either member on its own, or neither, is a start
/// the service accepts — the refusal is of the two TOGETHER, not of either one.
#[tokio::test]
async fn either_target_member_on_its_own_is_accepted() {
    let (router, engine) = service();
    seed_nodes(&engine);
    create_runbook(&router).await;

    let rows = [
        (
            "targets alone, bound to the runbook's parameter",
            Inputs {
                target_parameter_name: Some(TARGET_PARAMETER),
                targets: Some(json!([{"Key": "InstanceIds", "Values": [EC2_ONE, EC2_TWO]}])),
                target_maps: None,
            },
        ),
        (
            "target maps alone",
            Inputs {
                target_parameter_name: None,
                targets: None,
                target_maps: Some(target_maps(2)),
            },
        ),
        (
            "an empty target list beside maps, which names no target at all",
            Inputs {
                target_parameter_name: None,
                targets: Some(json!([])),
                target_maps: Some(target_maps(1)),
            },
        ),
        (
            "neither member",
            Inputs {
                target_parameter_name: None,
                targets: None,
                target_maps: None,
            },
        ),
    ];

    for (case, inputs) in &rows {
        let (status, started) = call(
            &router,
            "StartAutomationExecution",
            &automation_start_body(inputs),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected StartAutomationExecution to be accepted, \
             got {status} with {started}"
        );
        assert_eq!(
            text(&started, "AutomationExecutionId").len(),
            36,
            "case {case:?}: expected a minted execution id, got {started}"
        );

        let (status, preview) =
            call(&router, "StartExecutionPreview", &preview_start_body(inputs)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected StartExecutionPreview to be accepted, \
             got {status} with {preview}"
        );
        let preview_id = text(&preview, "ExecutionPreviewId");
        assert_eq!(
            preview_id.len(),
            36,
            "case {case:?}: expected a minted preview id, got {preview}"
        );

        // The preview the start persisted is reported back, and once the span the service
        // owes it has passed it is worked out.
        work_out(&engine);
        let (status, reported) = call(
            &router,
            "GetExecutionPreview",
            &json!({"ExecutionPreviewId": preview_id}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the preview to be reported back, got {status} with {reported}"
        );
        assert_eq!(
            text(&reported, "Status"),
            "Success",
            "case {case:?}: expected a worked-out preview, got {reported}"
        );
    }
}

/// A read of a preview id this Region holds no live preview for is refused as the modelled
/// miss, with the one sentence that covers both an id nothing was minted under and one whose
/// preview is gone — the id asked for, single-quoted, and no terminating period.
#[tokio::test]
async fn a_read_of_an_unknown_preview_id_says_it_does_not_exist_or_is_expired() {
    let (router, _engine) = service();

    // Each row is a well-formed `ExecutionPreviewId` (@length 36..36 in the UUID form) that
    // names no preview: the case numbers, the letters and the mixed case the modelled
    // pattern `^[a-fA-F0-9]{8}-...` admits.
    let rows = [
        ("all digits", "11111111-2222-3333-4444-555555555555"),
        ("lower-case hex letters", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
        ("upper-case hex letters", "ABCDEF01-2345-6789-ABCD-EF0123456789"),
        ("mixed case and digits", "0f9E8d7C-6b5a-4321-aB09-fedCBA987654"),
    ];

    for (case, preview_id) in rows {
        let (status, response) = call(
            &router,
            "GetExecutionPreview",
            &json!({"ExecutionPreviewId": preview_id}),
        )
        .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: expected HTTP 400 ResourceNotFoundException, \
             got {status} with {response}"
        );
        assert_eq!(
            error_message(&response),
            format!("Execution preview '{preview_id}' does not exist or is expired"),
            "case {case:?}: expected the refusal to name the id asked for, got {response}"
        );
    }
}
