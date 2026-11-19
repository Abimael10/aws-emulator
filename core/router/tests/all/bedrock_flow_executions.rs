// SPDX-License-Identifier: CC0-1.0

//! The Bedrock FLOW EXECUTION routes, inside the `/flows/...` URI space the two Bedrock
//! agent models SHARE: three services, one lifecycle.
//!
//! RULE: a flow, its preparation and its aliases belong to the Bedrock Agent BUILD-TIME
//! plane, while an EXECUTION of that flow belongs to the Bedrock Agent RUNTIME plane — and
//! both planes bind paths under `/flows/`. So, with the whole emulator assembled, a client
//! that creates and prepares a flow through the build-time plane can start an execution of
//! it, read that execution back and stop it through the runtime plane, and the build-time
//! plane keeps answering its own routes in the same space throughout.
//!
//! Two services have to agree for that to hold (which plane claims each path, and whose
//! store each half of the lifecycle lives in), so this is the suite that can pin it. The
//! per-operation rules of the execution lifecycle — the identifier forms, the uniqueness of
//! a `flowExecutionName`, the status the clock owes, the refusals — are the runtime plane's
//! own, and
//! `services/service-com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice/tests/all/agent_and_flow_invocations.rs`
//! is where they are tested.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state as agent_state;

/// The region every request here signs for, which is the region a minted execution ARN
/// carries.
const REGION: &str = "us-west-2";

/// The account every ARN this emulator mints carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The long-term key every emulator publishes: the account's own identity, which every
/// operation of these planes allows.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The working version of a flow, which the alias below routes to.
const DRAFT: &str = "DRAFT";

/// An IAM role ARN matching the build-time plane's modelled `FlowExecutionRoleArn`.
const FLOW_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForFlows";

/// The two services this lifecycle spans, over ONE engine, as `core/router/src/lib.rs`
/// mounts them. The engine is handed back so the suite can move an instant the clock owes
/// into the past instead of waiting for it.
fn build_router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite signs nothing: the `Authorization` header carries only the credential scope,
    // which is where a service reads the caller's access key id and region from.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_bedrockagentruntime_amazonbedrockagentruntimeservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// One restJson1 call to either Bedrock plane, returning `(status, body)`.
async fn bedrock(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match body {
        Some(value) => Bytes::from(serde_json::to_vec(&value).expect("the test body serializes")),
        None => Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/bedrock/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(payload)
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// A passthrough flow definition: one `Input` node wired straight to one `Output` node. The
/// node types, the union keys and the IO data types are the build-time model's own
/// `FlowNodeType` / `FlowNodeConfiguration` / `FlowNodeIODataType` values.
fn passthrough_definition() -> Value {
    json!({
        "nodes": [
            {
                "name": "FlowInput",
                "type": "Input",
                "configuration": { "input": {} },
                "outputs": [{ "name": "document", "type": "String" }],
            },
            {
                "name": "FlowOutput",
                "type": "Output",
                "configuration": { "output": {} },
                "inputs": [{ "name": "document", "type": "String", "expression": "$.data" }],
            },
        ],
        "connections": [
            {
                "name": "InputToOutput",
                "type": "Data",
                "source": "FlowInput",
                "target": "FlowOutput",
                "configuration": {
                    "data": { "sourceOutput": "document", "targetInput": "document" },
                },
            },
        ],
    })
}

/// A flow created and PREPARED through the build-time plane's own operations, with an alias
/// routing to the version it prepared: `(flowId, aliasId)`.
async fn prepared_flow(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
) -> (String, String) {
    let (status, created) = bedrock(
        router,
        "POST",
        "/flows/",
        Some(json!({
            "name": name,
            "executionRoleArn": FLOW_ROLE,
            "definition": passthrough_definition(),
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: setup CreateFlow must answer 201, got {status} with {created}"
    );
    let flow_id = created["id"]
        .as_str()
        .expect("a minted flowId")
        .to_string();

    let (status, prepared) = bedrock(
        router,
        "POST",
        &format!("/flows/{flow_id}/"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: setup PrepareFlow must answer 202, got {status} with {prepared}"
    );
    // The preparation the build-time plane's clock owes, moved into the past so the flow
    // stands at `Prepared` without the suite waiting for the span. Scoped, so the store's
    // guard is released before the next call.
    {
        let state = agent_state::service_state(&**engine)
            .expect("the Bedrock Agent store is on this engine");
        let mut store = state.lock();
        let pending = store
            .flows
            .get_mut(&flow_id)
            .expect("the stored flow")
            .pending_preparation
            .as_mut()
            .unwrap_or_else(|| panic!("case {case:?}: expected a preparation in flight"));
        pending.due = SystemTime::now() - Duration::from_secs(1);
    }

    let (status, alias) = bedrock(
        router,
        "POST",
        &format!("/flows/{flow_id}/aliases/"),
        Some(json!({
            "name": "deployed",
            "routingConfiguration": [{ "flowVersion": DRAFT }],
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: setup CreateFlowAlias must answer 201, got {status} with {alias}"
    );
    (
        flow_id,
        alias["id"].as_str().expect("a minted aliasId").to_string(),
    )
}

/// The flow one plane prepared is EXECUTABLE through the other: the runtime plane claims the
/// deeper paths of the shared `/flows/...` space and serves the whole lifecycle over the
/// build-time plane's flow, while the build-time plane keeps answering its own route in that
/// space.
#[tokio::test]
async fn a_flow_the_build_time_plane_prepared_is_executable_through_the_runtime_plane() {
    let case = "the execution of a prepared flow";
    let (router, engine) = build_router();
    let (flow_id, alias_id) = prepared_flow(&router, &engine, case, "cross-plane-flow").await;

    let (status, started) = bedrock(
        &router,
        "POST",
        &format!("/flows/{flow_id}/aliases/{alias_id}/executions"),
        Some(json!({
            "flowExecutionName": "cross-plane-run",
            "inputs": [{
                "nodeName": "FlowInput",
                "nodeOutputName": "document",
                "content": { "document": { "data": "the client's own input" } },
            }],
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the runtime plane to start an execution of the prepared \
         flow, got {status} with {started}"
    );
    assert_eq!(
        started["executionArn"].as_str(),
        Some(
            format!(
                "arn:aws:bedrock:{REGION}:{ACCOUNT}:flow/{flow_id}/alias/{alias_id}/execution/\
                 cross-plane-run"
            )
            .as_str()
        ),
        "case {case:?}: expected the execution ARN this emulator mints, got {started}"
    );

    let (status, read) = bedrock(
        &router,
        "GET",
        &format!("/flows/{flow_id}/aliases/{alias_id}/executions/cross-plane-run"),
        None,
    )
    .await;
    assert_eq!(
        (status, read["status"].as_str(), read["flowVersion"].as_str()),
        (200, Some("Running"), Some(DRAFT)),
        "case {case:?}: expected the runtime plane to report the execution it started, \
         running the version the alias routes to, got {read}"
    );

    let (status, stopped) = bedrock(
        &router,
        "POST",
        &format!("/flows/{flow_id}/aliases/{alias_id}/executions/cross-plane-run/stop"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        (status, stopped["status"].as_str()),
        (200, Some("Aborted")),
        "case {case:?}: expected the stop to abort the run, got {status} with {stopped}"
    );

    // The build-time plane still answers ITS route in the same space: the alias path one
    // segment shallower than the executions path is its `GetFlowAlias`.
    let (status, alias) = bedrock(
        &router,
        "GET",
        &format!("/flows/{flow_id}/aliases/{alias_id}"),
        None,
    )
    .await;
    assert_eq!(
        (status, alias["id"].as_str()),
        (200, Some(alias_id.as_str())),
        "case {case:?}: expected the build-time plane to keep serving its own route in the \
         shared flows space, got {status} with {alias}"
    );
}
