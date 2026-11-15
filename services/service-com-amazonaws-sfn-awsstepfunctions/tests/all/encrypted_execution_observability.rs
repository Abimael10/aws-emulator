// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what an execution REPORTS about itself when its data is held at
//! rest under a customer managed KMS key: Step Functions, KMS, CloudWatch Logs and AWS
//! X-Ray mounted on ONE engine, so the key a state machine names is the key KMS's own
//! handlers hold, the delivered history events are the ones CloudWatch Logs' own reader
//! answers with, and the trace is the one X-Ray's own `BatchGetTraces` returns.
//!
//! The RULE under test: **a state machine's `encryptionConfiguration` decides only how
//! its data is held at rest, never what a run of it reports — an execution of a state
//! machine whose `loggingConfiguration` names a log group delivers its history events to
//! that group, carrying the execution's data in the clear where
//! `includeExecutionData` asks for it, and an execution of a state machine whose
//! `tracingConfiguration` is enabled has its segments published under its chain at X-Ray,
//! identically whether that state machine is on a customer managed key or on the Amazon
//! Web Services owned key.** Neither sink is added by encryption either: a run of an
//! encrypted state machine that is configured to log nothing and to trace nothing reaches
//! neither.
//!
//! The rows differ along what the rule generalises over: HOW the data is held at rest (a
//! customer managed key with a data-key reuse period, one with none named, the AWS owned
//! key named outright, no configuration at all) and WHICH WAY the run was started (a
//! synchronous Express run, which ends inside the call, and a Standard run whose outcome
//! the clock owes).
//!
//! Each wait here is for the CONDITION — the records and the segment are looked for again
//! until they are there — so nothing depends on how fast the host runs; the deadline is
//! only a backstop against a publication that never happens.
//!
//! The key policies (`granting_policy`) and the encryption configuration
//! (`customer_managed`) are `execution_data_encryption`'s, and the log destination
//! (`logging`) is `execution_observability`'s; `tests/all.rs` is this binary's crate
//! root, so both are reached through `crate::<suite>`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::execution_data_encryption::{customer_managed, granting_policy, Wire, KMS, SFN};
use crate::execution_observability::logging;
use crate::state_machine_lifecycle::{string, HELLO_DEFINITION, REGION, ROLE_ARN};

/// How CloudWatch Logs' requests are addressed on the wire, from its own model's
/// `service_info` (`sigv4Name` and the `awsJson` version its protocol declares).
const LOGS: Wire = Wire {
    signing_name: "logs",
    target_prefix: "Logs_20140328",
    json_version: "1.1",
};

/// The service principal a log delivery is made by, which the issue's key policy grants
/// `kms:Decrypt` to: the key policy is the caller's own document, so it is carried here
/// exactly as a client writes it.
const LOG_DELIVERY_PRINCIPAL: &str = "delivery.logs.amazonaws.com";

/// The `origin` the segment of a Step Functions execution carries, spelled out here
/// rather than imported from the implementation, so a wrong constant in the service
/// cannot make a test pass.
const STATE_MACHINE_ORIGIN: &str = "AWS::StepFunctions::StateMachine";

/// The two history event types every row asserts the delivery of, as
/// `smithy_query(query="shape",shape="HistoryEventType")` names them: the opening of the
/// run and the terminal one of a run that succeeded.
const EXECUTION_STARTED: &str = "ExecutionStarted";
const EXECUTION_SUCCEEDED: &str = "ExecutionSucceeded";

/// The input every run below is started with: a document whose marker is observable in
/// the delivered `ExecutionStarted` record, which is what tells an encrypted run's data
/// apart from no data at all.
const INPUT: &str = r#"{"marker":"sealed-and-logged"}"#;

/// The output `HELLO_DEFINITION`'s one `Pass` state produces, which the terminal record
/// carries.
const OUTPUT: &str = r#"{"msg":"hi"}"#;

/// The terminal status a run of that definition reaches, as the modelled
/// `SyncExecutionStatus`/`ExecutionStatus` name it.
const SUCCEEDED: &str = "SUCCEEDED";

/// The status a run that has not ended yet reports.
const RUNNING: &str = "RUNNING";

/// How long a row waits for a condition, and how often it looks. Generous: the deadline
/// is a backstop against a delivery that never happens, not a measurement.
const DEADLINE: Duration = Duration::from_secs(30);
const LOOK: Duration = Duration::from_millis(50);

/// Step Functions, KMS, CloudWatch Logs and AWS X-Ray over ONE engine — the shape
/// `core/router/src/lib.rs` mounts them in.
///
/// All four are mounted because the path under test crosses all four: the key a state
/// machine names is resolved in KMS's store, the history events are delivered into
/// CloudWatch Logs' store, and the run's segments are submitted to X-Ray's own
/// `PutTraceSegments` through the in-process callback.
struct Sealed {
    router: MultiServiceRouter,
}

impl Sealed {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_xray_awsxray::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
    /// routes the request. The signature itself is never checked here.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        )
    }

    /// One call over an `awsJson` wire, requiring a 200.
    async fn json(&self, wire: &Wire, operation: &str, body: &Value) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header(
                "host",
                format!("{}.{REGION}.amazonaws.com", wire.signing_name),
            )
            .header(
                "content-type",
                format!("application/x-amz-json-{}", wire.json_version),
            )
            .header("x-amz-target", format!("{}.{operation}", wire.target_prefix))
            .header("authorization", Self::authorization(wire.signing_name))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = parsed_body(response.into_body());
        assert_eq!(
            status, 200,
            "case {operation}: expected 200, got {status} with {parsed:?}"
        );
        parsed
    }

    /// One Step Functions call, requiring a 200.
    async fn sfn(&self, operation: &str, body: &Value) -> Value {
        self.json(&SFN, operation, body).await
    }

    /// One KMS call, requiring a 200.
    async fn kms(&self, operation: &str, body: &Value) -> Value {
        self.json(&KMS, operation, body).await
    }

    /// One CloudWatch Logs call, requiring a 200.
    async fn logs(&self, operation: &str, body: &Value) -> Value {
        self.json(&LOGS, operation, body).await
    }

    /// One X-Ray call over its `restJson1` wire, requiring a 200.
    async fn xray(&self, path: &str, body: &Value) -> Value {
        let host = format!("xray.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}{path}"))
            .header("host", &host)
            .header("content-type", "application/json")
            .header("authorization", Self::authorization("xray"))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = parsed_body(response.into_body());
        assert_eq!(
            status, 200,
            "case POST {path}: expected 200, got {status} with {parsed:?}"
        );
        parsed
    }

    /// A symmetric encryption key whose policy grants the account that owns it every
    /// cryptographic operation and the log-delivery principal the decrypt a vended log
    /// needs, as its key ARN.
    async fn create_key(&self) -> String {
        let created = self
            .kms(
                "CreateKey",
                &json!({
                    "KeySpec": "SYMMETRIC_DEFAULT",
                    "Policy": delivery_key_policy().to_string(),
                    "BypassPolicyLockoutSafetyCheck": true,
                }),
            )
            .await;
        string(&created["KeyMetadata"], "Arn")
    }

    /// A state machine of `sm_type` holding its data as `encryption` says, delivering its
    /// history at `level` to the group `group` and tracing as `traced` says. Answers its
    /// ARN.
    async fn given_state_machine(
        &self,
        name: &str,
        sm_type: &str,
        encryption: Option<Value>,
        level: &str,
        group: &str,
        traced: bool,
    ) -> String {
        let mut request = json!({
            "name": name,
            "definition": HELLO_DEFINITION,
            "roleArn": ROLE_ARN,
            "type": sm_type,
            "tracingConfiguration": { "enabled": traced },
        });
        if level == LOG_LEVEL_OFF {
            // A configuration that logs nothing names no destination: there is nothing
            // for it to deliver to.
            request["loggingConfiguration"] = json!({
                "level": LOG_LEVEL_OFF,
                "includeExecutionData": false,
            });
        } else {
            request["loggingConfiguration"] = logging(level, group, true);
        }
        if let Some(encryption) = encryption {
            request["encryptionConfiguration"] = encryption;
        }
        let created = self.sfn("CreateStateMachine", &request).await;
        string(&created, "stateMachineArn")
    }

    /// Every history record the group `group` holds for the execution `execution_arn`, as
    /// the parsed JSON message of each — the reader a client finds a vended event with.
    async fn records_of(&self, group: &str, execution_arn: &str) -> Vec<Value> {
        let found = self
            .logs("FilterLogEvents", &json!({ "logGroupName": group }))
            .await;
        found
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|event| event.get("message").and_then(Value::as_str))
            .filter_map(|message| serde_json::from_str::<Value>(message).ok())
            .filter(|record| {
                record.get("execution_arn").and_then(Value::as_str) == Some(execution_arn)
            })
            .collect()
    }

    /// Wait until the group holds a record of every type in `wanted` for this execution,
    /// and answer the records it then holds.
    async fn awaited_records(
        &self,
        case: &str,
        group: &str,
        execution_arn: &str,
        wanted: &[&str],
    ) -> Vec<Value> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let held = self.records_of(group, execution_arn).await;
            if wanted
                .iter()
                .all(|event_type| record_of(&held, event_type).is_some())
            {
                return held;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the group {group:?} never held {wanted:?} for the run \
                 {execution_arn:?}; it held {held:?}"
            );
            ::tokio::time::sleep(LOOK).await;
        }
    }

    /// Every segment document X-Ray holds under the trace `root`: `BatchGetTraces` for
    /// that chain, which is where a client looks for the trace of one run.
    async fn segments_of(&self, root: &str) -> Vec<Value> {
        let answered = self.xray("/Traces", &json!({ "TraceIds": [root] })).await;
        answered
            .get("Traces")
            .and_then(Value::as_array)
            .map(|traces| {
                traces
                    .iter()
                    .flat_map(|trace| {
                        trace
                            .get("Segments")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                    })
                    .filter_map(|segment| {
                        segment
                            .get("Document")
                            .and_then(Value::as_str)
                            .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The segment X-Ray holds under `root` for the execution `execution_arn`, or `None`
    /// where it holds none: a run's own segment names the execution it reports in its
    /// annotations.
    async fn segment_of(&self, root: &str, execution_arn: &str) -> Option<Value> {
        self.segments_of(root)
            .await
            .into_iter()
            .find(|document| annotation(document, "execution_arn").as_deref() == Some(execution_arn))
    }

    /// Wait until X-Ray holds the segment of `execution_arn` under `root`, and answer it.
    async fn awaited_segment(&self, case: &str, root: &str, execution_arn: &str) -> Value {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(segment) = self.segment_of(root, execution_arn).await {
                return segment;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: X-Ray never held a segment for the run {execution_arn:?} under \
                 the chain {root:?}"
            );
            ::tokio::time::sleep(LOOK).await;
        }
    }

    /// Wait until the execution `execution_arn` has left `RUNNING`, and answer the status
    /// it reached — for a run whose outcome the clock owes.
    async fn awaited_status(&self, case: &str, execution_arn: &str) -> String {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let described = self
                .sfn("DescribeExecution", &json!({ "executionArn": execution_arn }))
                .await;
            let status = string(&described, "status");
            if status != RUNNING {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the run {execution_arn:?} never left {RUNNING}"
            );
            ::tokio::time::sleep(LOOK).await;
        }
    }
}

/// The level that logs every history event, and the one that logs none, as `LogLevel`
/// names them (`smithy_query(query="shape",shape="LogLevel")`).
const LOG_LEVEL_ALL: &str = "ALL";
const LOG_LEVEL_OFF: &str = "OFF";

/// The two workflow types, as `StateMachineType` names them.
const EXPRESS: &str = "EXPRESS";
const STANDARD: &str = "STANDARD";

/// The body of a response, parsed, or `Null` for an empty one.
fn parsed_body(bytes: Bytes) -> Value {
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }
}

/// The account-delegating key policy with the log-delivery principal's decrypt beside it,
/// which is the document the steps of this rule's own report put on the key.
fn delivery_key_policy() -> Value {
    let mut policy = granting_policy();
    if let Some(statements) = policy.get_mut("Statement").and_then(Value::as_array_mut) {
        statements.push(json!({
            "Sid": "AllowLogDelivery",
            "Effect": "Allow",
            "Principal": { "Service": LOG_DELIVERY_PRINCIPAL },
            "Action": "kms:Decrypt",
            "Resource": "*"
        }));
    }
    policy
}

/// Every way a state machine's data is held at rest, which is what the rule generalises
/// over: `EncryptionType` is `AWS_OWNED_KEY | CUSTOMER_MANAGED_KMS_KEY`
/// (`smithy_query(query="shape",shape="EncryptionConfiguration")`), the latter with and
/// without the `kmsDataKeyReusePeriodSeconds` the member leaves optional, beside a state
/// machine created with no configuration at all.
fn at_rest(key: &str) -> Vec<(&'static str, Option<Value>)> {
    vec![
        (
            "a customer managed key with a data key reuse period",
            Some(customer_managed(key)),
        ),
        (
            "a customer managed key with no reuse period named",
            Some(json!({ "type": "CUSTOMER_MANAGED_KMS_KEY", "kmsKeyId": key })),
        ),
        (
            "the Amazon Web Services owned key named outright",
            Some(json!({ "type": "AWS_OWNED_KEY" })),
        ),
        ("no encryption configuration at all", None),
    ]
}

/// A chain of X-Ray's own form (`1-<8 hex>-<24 hex>`), distinct per row, so each row's
/// assertions are about the trace of its own run.
fn chain_for(index: usize) -> String {
    format!("1-5759e988-bd862e3fe1be46a9942727{index:02x}")
}

/// The trace header a caller supplies for `root`, with the sampling decision taken.
fn sampled_header(root: &str) -> String {
    format!("Root={root};Sampled=1")
}

/// The `Root` of a trace header, which is the chain it names.
fn root_of(header: &str) -> Option<String> {
    header
        .split(';')
        .filter_map(|field| field.trim().strip_prefix("Root="))
        .map(str::to_string)
        .next()
}

/// The record of type `event_type` among `records`, or `None` where there is none.
fn record_of<'a>(records: &'a [Value], event_type: &str) -> Option<&'a Value> {
    records
        .iter()
        .find(|record| record.get("type").and_then(Value::as_str) == Some(event_type))
}

/// The annotation `name` a segment document carries, as text.
fn annotation(document: &Value, name: &str) -> Option<String> {
    document
        .get("annotations")
        .and_then(|annotations| annotations.get(name))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The text at `pointer` inside `value`.
fn at(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// What both sinks must hold for a run that logged at `ALL` and traced: the opening
/// record carrying the input in the clear, the terminal record carrying the output, and
/// the run's own segment under the chain the caller named.
async fn assert_both_sinks(
    sealed: &Sealed,
    case: &str,
    group: &str,
    root: &str,
    execution_arn: &str,
    state_machine_arn: &str,
) {
    let records = sealed
        .awaited_records(
            case,
            group,
            execution_arn,
            &[EXECUTION_STARTED, EXECUTION_SUCCEEDED],
        )
        .await;
    let started = record_of(&records, EXECUTION_STARTED)
        .unwrap_or_else(|| panic!("case {case:?}: the awaited records hold the opening one"));
    assert_eq!(
        at(started, "/details/input").as_deref(),
        Some(INPUT),
        "case {case:?}: the delivered opening record carries the input the run was started \
         with, got {started:?}"
    );
    let succeeded = record_of(&records, EXECUTION_SUCCEEDED)
        .unwrap_or_else(|| panic!("case {case:?}: the awaited records hold the terminal one"));
    assert_eq!(
        at(succeeded, "/details/output").as_deref(),
        Some(OUTPUT),
        "case {case:?}: the delivered terminal record carries the output the run produced, got \
         {succeeded:?}"
    );

    let segment = sealed.awaited_segment(case, root, execution_arn).await;
    assert_eq!(
        segment.get("origin").and_then(Value::as_str),
        Some(STATE_MACHINE_ORIGIN),
        "case {case:?}: the published segment is the state machine's own, got {segment:?}"
    );
    assert_eq!(
        segment.get("resource_arn").and_then(Value::as_str),
        Some(state_machine_arn),
        "case {case:?}: the published segment names the state machine that ran it, got \
         {segment:?}"
    );
    assert_eq!(
        annotation(&segment, "status").as_deref(),
        Some(SUCCEEDED),
        "case {case:?}: the published segment reports the status the run reached, got {segment:?}"
    );
}

/// A SYNCHRONOUS Express run reaches both sinks whatever key holds its data: the history
/// records of the group its configuration names, with the execution's own data in the
/// clear, and the segment of its chain at X-Ray.
#[tokio::test]
async fn a_synchronous_run_reaches_both_sinks_whatever_key_seals_its_data() {
    let sealed = Sealed::new();
    let key = sealed.create_key().await;

    for (index, (case, encryption)) in at_rest(&key).into_iter().enumerate() {
        let group = format!("/aws/vendedlogs/states/sealed-sync-{index}");
        sealed
            .logs("CreateLogGroup", &json!({ "logGroupName": group }))
            .await;
        let machine = sealed
            .given_state_machine(
                &format!("sealed-sync-{index}"),
                EXPRESS,
                encryption,
                LOG_LEVEL_ALL,
                &group,
                true,
            )
            .await;

        let root = chain_for(index);
        let answered = sealed
            .sfn(
                "StartSyncExecution",
                &json!({
                    "stateMachineArn": machine,
                    "name": "run",
                    "input": INPUT,
                    "traceHeader": sampled_header(&root),
                }),
            )
            .await;
        assert_eq!(
            string(&answered, "status"),
            SUCCEEDED,
            "case {case:?}: the run itself succeeds, got {answered:?}"
        );
        // The echoed header names the chain the caller supplied, which is the chain its
        // trace is looked for under.
        assert_eq!(
            root_of(&string(&answered, "traceHeader")).as_deref(),
            Some(root.as_str()),
            "case {case:?}: the response echoes the chain the caller named, got {answered:?}"
        );
        let execution = string(&answered, "executionArn");

        assert_both_sinks(&sealed, case, &group, &root, &execution, &machine).await;
    }
}

/// The same for a STANDARD run, whose outcome the clock owes: the run is waited for, and
/// then both sinks hold what it reported.
#[tokio::test]
async fn an_asynchronous_run_reaches_both_sinks_whatever_key_seals_its_data() {
    let sealed = Sealed::new();
    let key = sealed.create_key().await;

    for (index, (case, encryption)) in at_rest(&key).into_iter().enumerate() {
        let group = format!("/aws/vendedlogs/states/sealed-async-{index}");
        sealed
            .logs("CreateLogGroup", &json!({ "logGroupName": group }))
            .await;
        let machine = sealed
            .given_state_machine(
                &format!("sealed-async-{index}"),
                STANDARD,
                encryption,
                LOG_LEVEL_ALL,
                &group,
                true,
            )
            .await;

        let root = chain_for(index);
        let started = sealed
            .sfn(
                "StartExecution",
                &json!({
                    "stateMachineArn": machine,
                    "name": "run",
                    "input": INPUT,
                    "traceHeader": sampled_header(&root),
                }),
            )
            .await;
        let execution = string(&started, "executionArn");
        let status = sealed.awaited_status(case, &execution).await;
        assert_eq!(
            status, SUCCEEDED,
            "case {case:?}: the run itself succeeds, got {status:?}"
        );

        assert_both_sinks(&sealed, case, &group, &root, &execution, &machine).await;
    }
}

/// Encryption adds NO sink the configuration does not ask for: a run of a customer
/// managed key state machine that logs nothing and traces nothing reaches neither — shown
/// once its twin on the SAME key, configured to log and to trace, has reached both, so an
/// absent record is one that was never delivered rather than one still in flight.
#[tokio::test]
async fn an_encrypted_run_reaches_no_sink_its_configuration_withholds() {
    let sealed = Sealed::new();
    let key = sealed.create_key().await;

    let group = "/aws/vendedlogs/states/sealed-silent";
    sealed
        .logs("CreateLogGroup", &json!({ "logGroupName": group }))
        .await;

    let silent = sealed
        .given_state_machine(
            "sealed-silent",
            EXPRESS,
            Some(customer_managed(&key)),
            LOG_LEVEL_OFF,
            group,
            false,
        )
        .await;
    let loud = sealed
        .given_state_machine(
            "sealed-loud",
            EXPRESS,
            Some(customer_managed(&key)),
            LOG_LEVEL_ALL,
            group,
            true,
        )
        .await;

    let root = chain_for(0);
    let quiet_run = sealed
        .sfn(
            "StartSyncExecution",
            &json!({
                "stateMachineArn": silent,
                "name": "run",
                "input": INPUT,
                "traceHeader": sampled_header(&root),
            }),
        )
        .await;
    assert_eq!(
        string(&quiet_run, "status"),
        SUCCEEDED,
        "case the silent twin: the run itself succeeds, got {quiet_run:?}"
    );
    let quiet = string(&quiet_run, "executionArn");

    // The positive control, on the same key and the same chain: once its records and its
    // segment are there, everything either run owed either sink has gone out.
    let control_run = sealed
        .sfn(
            "StartSyncExecution",
            &json!({
                "stateMachineArn": loud,
                "name": "run",
                "input": INPUT,
                "traceHeader": sampled_header(&root),
            }),
        )
        .await;
    let control = string(&control_run, "executionArn");
    assert_both_sinks(&sealed, "the control twin", group, &root, &control, &loud).await;

    // The group is readable and the chain is retrievable — the control's own records and
    // segment are in them — so what follows is about the silent run and not about a sink
    // nothing could be read from.
    let held = sealed.records_of(group, &quiet).await;
    assert!(
        held.is_empty(),
        "case the silent twin: a configuration that logs nothing delivers nothing, got {held:?}"
    );
    let segment = sealed.segment_of(&root, &quiet).await;
    assert!(
        segment.is_none(),
        "case the silent twin: a state machine that is not tracing publishes nothing, got \
         {segment:?}"
    );
}
