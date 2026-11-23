// SPDX-License-Identifier: CC0-1.0

//! End-to-end behaviour of `GetDataIntegrationFlowExecution`, driven over the real wire
//! path (a restJson1 request through the multi-service router into the mounted service), so
//! every assertion is about what a client observes.
//!
//! RULES under test:
//!
//! * the read reports exactly the flow-execution record the store holds for the addressed
//!   triple — every member of it, and nothing invented for the members a run left absent;
//! * the gateway authorizes an instance-scoped call against the INSTANCE before it looks
//!   the addressed resource up, so an `instanceId` naming no instance of this account is
//!   `AccessDeniedException` (HTTP 403) carrying the gateway's authorization sentence and
//!   NOT a not-found — whatever flow or execution id the request named;
//! * within a live instance, a flow the instance does not hold and an `executionId` that
//!   flow does not hold are each `ResourceNotFoundException` (HTTP 404), with the
//!   sentence of the resource that is missing;
//! * the read changes nothing a later caller sees.
//!
//! The executions the cases read are seeded into the shared store through the same
//! `state::service_state` accessor the handler reads it through: a flow execution is
//! created by the flow RUNNING — the model binds no operation that starts one — so the
//! store is the only producer available to a test of the READ. The read therefore answers
//! from persisted records, which is what every assertion below pins.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::ops::types::{
    DataIntegrationFlowDatasetSource, DataIntegrationFlowExecutionOutputMetadata,
    DataIntegrationFlowExecutionSourceInfo, DataIntegrationFlowS3Source,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::state::{
    flow_execution_key, service_state, DataIntegrationFlowExecutionRecord,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these cases sign for.
const REGION: &str = "us-east-1";

/// The canonical account id this emulator serves (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// The sentences this service answers these refusals with, written out HERE rather than
/// read from the crate: the wording is the requirement, so a drift in what the handler
/// answers fails this suite instead of travelling with it. The flow's own not-found names
/// the flow and its instance, the two `@httpLabel` members of the flow URI; the
/// execution's is fixed.
fn expected_flow_not_found_sentence(flow_name: &str, instance_id: &str) -> String {
    format!("Flow {flow_name} does not exist in instance {instance_id}")
}
const FLOW_EXECUTION_NOT_FOUND_SENTENCE: &str = "Flow execution not found.";

/// The emulator under test: one engine, one service, driven through the router.
struct Emulator {
    router: MultiServiceRouter,
    /// The SAME engine the service was built from, so a case can seed the execution store
    /// and read it back the way the handler does.
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// Send one request and return its HTTP status and JSON body.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let body_bytes = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value).expect("serialize request body")),
            None => Bytes::new(),
        };
        let credential = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/scn/aws4_request, \
             SignedHeaders=host, Signature=00"
        );
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://scn.{REGION}.amazonaws.com{path}"))
            .header("host", format!("scn.{REGION}.amazonaws.com"))
            .header("authorization", credential)
            .header("content-type", "application/json")
            .body(body_bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "{method} {path}: body is not JSON ({error}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, parsed)
    }

    /// A live instance of this account, returning its `instanceId`.
    async fn given_instance(&self) -> String {
        let (status, body) = self.send("POST", "/api/instance", Some(json!({}))).await;
        assert_eq!(
            status, 200,
            "CreateInstance expected 200, got {status} with {body}"
        );
        body["instance"]["instanceId"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateInstance carried no instanceId: {body}"))
            .to_string()
    }

    /// A flow of `instance_id`, created through `CreateDataIntegrationFlow`.
    async fn given_flow(&self, instance_id: &str, name: &str) {
        let (status, body) = self
            .send(
                "PUT",
                &format!(
                    "/api/data-integration/instance/{instance_id}/data-integration-flows/{name}"
                ),
                Some(json!({
                    "sources": [{
                        "sourceType": "S3",
                        "sourceName": "orders",
                        "s3Source": { "bucketName": "supply-landing", "prefix": "orders/" },
                    }],
                    "transformation": { "transformationType": "NONE" },
                    "target": {
                        "targetType": "S3",
                        "s3Target": { "bucketName": "supply-exports", "prefix": "orders/" },
                    },
                })),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateDataIntegrationFlow expected 200, got {status} with {body}"
        );
    }

    /// Record one run of a flow in the shared store, as the flow running records one.
    fn given_execution(&self, record: DataIntegrationFlowExecutionRecord) {
        let state = service_state(&*self.engine).expect("the supplychain store");
        state.lock().flow_executions.insert(
            flow_execution_key(&record.instance_id, &record.flow_name, &record.execution_id),
            record,
        );
    }

    /// `GetDataIntegrationFlowExecution` for one triple.
    async fn get_execution(
        &self,
        instance_id: &str,
        flow_name: &str,
        execution_id: &str,
    ) -> (u16, Value) {
        self.send(
            "GET",
            &format!(
                "/api-data/data-integration/instance/{instance_id}/data-integration-flows/\
                 {flow_name}/executions/{execution_id}"
            ),
            None,
        )
        .await
    }

    /// `DeleteDataIntegrationFlow` for one (instance, flow) pair.
    async fn delete_flow(&self, instance_id: &str, name: &str) -> (u16, Value) {
        self.send(
            "DELETE",
            &format!(
                "/api/data-integration/instance/{instance_id}/data-integration-flows/{name}"
            ),
            None,
        )
        .await
    }
}

/// The error code a refusal envelope carries.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The sentence a refusal envelope carries.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// The sentence the gateway refuses an instance-scoped call naming an unknown instance
/// with — fixed except for the `instanceId` the request carried and the account the
/// request was served for.
fn expected_authorization_failure(instance_id: &str) -> String {
    format!(
        "Authorization Failed (Public). The instanceId {instance_id} does not exist, or does not \
         belong to the account {ACCOUNT}"
    )
}

/// An `executionId` of the modelled `UUID` form for `index`.
fn execution_id(index: u64) -> String {
    format!("{index:08x}-0000-4000-8000-{index:012x}")
}

/// RULE: the read reports exactly the stored run — every member it holds, and nothing for
/// the members a run left absent — and it changes nothing a later caller sees.
#[tokio::test]
async fn a_stored_flow_execution_is_reported_exactly_as_it_was_persisted() {
    let emulator = Emulator::new();
    let instance_id = emulator.given_instance().await;
    emulator.given_flow(&instance_id, "orders-flow").await;
    let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let ended = started + Duration::from_secs(90);

    // Rows span the shapes a run takes: one still in progress with an S3 source and no end
    // or output, one that succeeded from a dataset source with its output metadata, and one
    // that failed and carries the failure message.
    let rows = vec![
        (
            "a run still in progress",
            DataIntegrationFlowExecutionRecord {
                instance_id: instance_id.clone(),
                flow_name: "orders-flow".to_string(),
                execution_id: execution_id(1),
                status: Some("IN_PROGRESS".to_string()),
                source_info: Some(DataIntegrationFlowExecutionSourceInfo {
                    source_type: "S3".to_string(),
                    s3_source: Some(DataIntegrationFlowS3Source {
                        bucket_name: "supply-landing".to_string(),
                        key: "orders/2026-01-01.csv".to_string(),
                    }),
                    dataset_source: None,
                }),
                message: None,
                start_time: Some(started),
                end_time: None,
                output_metadata: None,
            },
            json!({
                "instanceId": instance_id,
                "flowName": "orders-flow",
                "executionId": execution_id(1),
                "status": "IN_PROGRESS",
                "sourceInfo": {
                    "sourceType": "S3",
                    "s3Source": {
                        "bucketName": "supply-landing",
                        "key": "orders/2026-01-01.csv",
                    },
                },
                "startTime": 1_700_000_000,
            }),
        ),
        (
            "a run that succeeded from a dataset source",
            DataIntegrationFlowExecutionRecord {
                instance_id: instance_id.clone(),
                flow_name: "orders-flow".to_string(),
                execution_id: execution_id(2),
                status: Some("SUCCEEDED".to_string()),
                source_info: Some(DataIntegrationFlowExecutionSourceInfo {
                    source_type: "DATASET".to_string(),
                    s3_source: None,
                    dataset_source: Some(DataIntegrationFlowDatasetSource {
                        dataset_identifier: format!(
                            "arn:aws:scn:{REGION}:{ACCOUNT}:instance/{instance_id}/namespaces/\
                             default/datasets/shipments"
                        ),
                    }),
                }),
                message: None,
                start_time: Some(started),
                end_time: Some(ended),
                output_metadata: Some(DataIntegrationFlowExecutionOutputMetadata {
                    diagnostic_reports_root_s3_uri: Some(
                        "s3://supply-exports/diagnostics/".to_string(),
                    ),
                }),
            },
            json!({
                "instanceId": instance_id,
                "flowName": "orders-flow",
                "executionId": execution_id(2),
                "status": "SUCCEEDED",
                "sourceInfo": {
                    "sourceType": "DATASET",
                    "datasetSource": {
                        "datasetIdentifier": format!(
                            "arn:aws:scn:{REGION}:{ACCOUNT}:instance/{instance_id}/namespaces/\
                             default/datasets/shipments"
                        ),
                    },
                },
                "startTime": 1_700_000_000,
                "endTime": 1_700_000_090,
                "outputMetadata": {
                    "diagnosticReportsRootS3URI": "s3://supply-exports/diagnostics/",
                },
            }),
        ),
        (
            "a run that failed, with its message",
            DataIntegrationFlowExecutionRecord {
                instance_id: instance_id.clone(),
                flow_name: "orders-flow".to_string(),
                execution_id: execution_id(3),
                status: Some("FAILED".to_string()),
                source_info: None,
                message: Some("the source object could not be read".to_string()),
                start_time: Some(started),
                end_time: Some(ended),
                output_metadata: None,
            },
            json!({
                "instanceId": instance_id,
                "flowName": "orders-flow",
                "executionId": execution_id(3),
                "status": "FAILED",
                "message": "the source object could not be read",
                "startTime": 1_700_000_000,
                "endTime": 1_700_000_090,
            }),
        ),
    ];

    for (case, record, expected) in rows {
        let id = record.execution_id.clone();
        emulator.given_execution(record);
        let (status, body) = emulator.get_execution(&instance_id, "orders-flow", &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
        assert_eq!(
            body.get("flowExecution"),
            Some(&expected),
            "case {case:?}: the read must report exactly the stored run, got {body}"
        );

        // A read is a report: looking again answers identically.
        let (again_status, again_body) = emulator
            .get_execution(&instance_id, "orders-flow", &id)
            .await;
        assert_eq!(
            (again_status, &again_body),
            (status, &body),
            "case {case:?}: a repeated read must answer identically, got {again_body}"
        );
    }
}

/// RULE: an `instanceId` naming no instance of this account is refused by the
/// authorization step — before the flow or the execution is looked up at all — so the
/// answer is 403 `AccessDeniedException` with the gateway's sentence, whatever the rest of
/// the request names.
#[tokio::test]
async fn an_instance_this_account_does_not_hold_is_an_authorization_failure() {
    let emulator = Emulator::new();
    let live = emulator.given_instance().await;
    emulator.given_flow(&live, "orders-flow").await;
    emulator.given_execution(DataIntegrationFlowExecutionRecord {
        instance_id: live.clone(),
        flow_name: "orders-flow".to_string(),
        execution_id: execution_id(1),
        status: Some("SUCCEEDED".to_string()),
        source_info: None,
        message: None,
        start_time: None,
        end_time: None,
        output_metadata: None,
    });

    // Rows differ along WHY the instance is not this account's, and along what the rest of
    // the request names: a flow and execution that exist under the LIVE instance, and ones
    // that exist nowhere.
    let deleted = emulator.given_instance().await;
    let (status, body) = emulator
        .send("DELETE", &format!("/api/instance/{deleted}"), None)
        .await;
    assert_eq!(
        status, 200,
        "DeleteInstance expected 200, got {status} with {body}"
    );
    // An instance is this account's until its accepted deletion settles, so the row about a
    // deleted instance waits for the removal the clock owes.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let (status, _) = emulator
            .send("GET", &format!("/api/instance/{deleted}"), None)
            .await;
        if status == 404 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the deletion of {deleted} never settled"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let unknown = "00000000-0000-4000-8000-000000000000".to_string();
    for (case, instance_id) in [
        ("an instance nothing created", unknown),
        ("an instance this account deleted", deleted),
    ] {
        for (what, flow_name, id) in [
            ("a flow and execution that exist elsewhere", "orders-flow", execution_id(1)),
            ("a flow and execution that exist nowhere", "sim2real-nonexistent-flow", execution_id(9)),
        ] {
            let (status, body) = emulator.get_execution(&instance_id, flow_name, &id).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (403, "AccessDeniedException"),
                "case {case:?} with {what}: expected 403 AccessDeniedException, got {status} \
                 with {body}"
            );
            assert_eq!(
                error_message(&body),
                expected_authorization_failure(&instance_id),
                "case {case:?} with {what}: the refusal must carry the gateway's authorization \
                 sentence, got {body}"
            );
        }
    }
}

/// RULE: within a live instance, the missing resource is what the refusal names: a flow the
/// instance does not hold is the flow's not-found, naming the flow and its instance, and an
/// `executionId` the flow does not hold is the execution's — each a 404.
#[tokio::test]
async fn a_missing_flow_or_execution_is_the_not_found_of_what_is_missing() {
    let emulator = Emulator::new();
    let instance_id = emulator.given_instance().await;
    emulator.given_flow(&instance_id, "orders-flow").await;
    emulator.given_execution(DataIntegrationFlowExecutionRecord {
        instance_id: instance_id.clone(),
        flow_name: "orders-flow".to_string(),
        execution_id: execution_id(1),
        status: Some("SUCCEEDED".to_string()),
        source_info: None,
        message: None,
        start_time: None,
        end_time: None,
        output_metadata: None,
    });

    // Rows: (case, the flow the request names, the execution it names, the sentence).
    let rows = [
        (
            "a flow that was never created",
            "never-created",
            execution_id(1),
            expected_flow_not_found_sentence("never-created", &instance_id),
        ),
        (
            "a flow name one character off a live one",
            "orders-flo",
            execution_id(1),
            expected_flow_not_found_sentence("orders-flo", &instance_id),
        ),
        (
            "a live flow and an execution it does not hold",
            "orders-flow",
            execution_id(2),
            FLOW_EXECUTION_NOT_FOUND_SENTENCE.to_string(),
        ),
        (
            "a live flow and the lowest execution id the shape admits",
            "orders-flow",
            "00000000-0000-0000-0000-000000000000".to_string(),
            FLOW_EXECUTION_NOT_FOUND_SENTENCE.to_string(),
        ),
    ];

    for (case, flow_name, id, sentence) in rows {
        let (status, body) = emulator.get_execution(&instance_id, flow_name, &id).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected 404 ResourceNotFoundException, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            sentence,
            "case {case:?}: the refusal must carry the not-found sentence of the resource \
             that is missing, got {body}"
        );
        assert!(
            !error_message(&body).contains(&id),
            "case {case:?}: no sentence of this family carries the execution id, got {body}"
        );
    }

    // The run that IS stored is still readable: none of the refusals touched the store.
    let (status, body) = emulator
        .get_execution(&instance_id, "orders-flow", &execution_id(1))
        .await;
    assert_eq!(
        status, 200,
        "case \"the stored run survives\": expected 200, got {status} with {body}"
    );
}

/// RULE: a run is addressed THROUGH its flow, so it does not outlive the flow whose delete
/// removed it: after `DeleteDataIntegrationFlow` every read of one of that flow's runs is
/// the flow's own not-found, and re-creating the name gives a flow holding none of the
/// runs the removed one held.
#[tokio::test]
async fn a_run_does_not_outlive_the_flow_whose_delete_removed_it() {
    let emulator = Emulator::new();
    let instance_id = emulator.given_instance().await;

    // Two flows of one instance, each holding runs, so the delete has a flow to leave
    // alone as well as one to remove.
    for flow_name in ["orders-flow", "returns-flow"] {
        emulator.given_flow(&instance_id, flow_name).await;
        for index in [1, 2] {
            emulator.given_execution(DataIntegrationFlowExecutionRecord {
                instance_id: instance_id.clone(),
                flow_name: flow_name.to_string(),
                execution_id: execution_id(index),
                status: Some("SUCCEEDED".to_string()),
                source_info: None,
                message: None,
                start_time: None,
                end_time: None,
                output_metadata: None,
            });
        }
    }

    let (status, body) = emulator.delete_flow(&instance_id, "orders-flow").await;
    assert_eq!(
        status, 200,
        "case \"the delete\": expected 200, got {status} with {body}"
    );

    // Rows: each (flow, run) pair that existed before the delete, and what the read
    // answers for it now.
    for (case, flow_name, index, expected) in [
        ("a run of the deleted flow", "orders-flow", 1, 404),
        ("its second run", "orders-flow", 2, 404),
        ("a run of the flow left alone", "returns-flow", 1, 200),
        ("its second run", "returns-flow", 2, 200),
    ] {
        let (status, body) = emulator
            .get_execution(&instance_id, flow_name, &execution_id(index))
            .await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected {expected}, got {status} with {body}"
        );
        if expected == 404 {
            assert_eq!(
                error_message(&body),
                expected_flow_not_found_sentence(flow_name, &instance_id),
                "case {case:?}: the refusal must name the flow that is missing, got {body}"
            );
        }
    }

    // The name is free again, and the flow created under it holds none of the removed
    // flow's runs: the read is the EXECUTION's not-found, not the flow's.
    emulator.given_flow(&instance_id, "orders-flow").await;
    for index in [1, 2] {
        let (status, body) = emulator
            .get_execution(&instance_id, "orders-flow", &execution_id(index))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case \"re-created flow, run {index}\": expected 404 ResourceNotFoundException, \
             got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            FLOW_EXECUTION_NOT_FOUND_SENTENCE,
            "case \"re-created flow, run {index}\": a run of the removed flow must not come \
             back, got {body}"
        );
    }
}
