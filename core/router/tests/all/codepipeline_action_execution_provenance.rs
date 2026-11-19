// SPDX-License-Identifier: CC0-1.0

//! What a CodePipeline action execution records about WHO performed it and WHAT the
//! provider that performed it called the work — read back over the wire from
//! `ListActionExecutions` and `GetPipelineState` — and the resolution every timestamp
//! this service serializes carries.
//!
//! The rules under test, each asserted over a SET of inputs rather than one case:
//!
//! * `ActionExecutionResult$externalExecutionId` is "the action provider's external ID for
//!   the action execution", so every finished action execution whose work a provider
//!   performed reports the id that provider names the work by: for a SOURCE action the
//!   revision it delivered (the object's version where the bucket holds versions, its
//!   entity tag where it does not — the same value the action publishes as its own output
//!   variable), and for the manual-approval provider, whose record of the decision IS the
//!   action execution, that execution's own id;
//! * `ActionExecutionInput$roleArn` is "the ARN of the IAM service role that performs the
//!   declared action[, which] is assumed through the roleArn for the pipeline", so it is
//!   reported for an action a service role performs — the action's own role when it
//!   declares one, else the pipeline's — and not at all for an `Approval` action, which a
//!   PRINCIPAL answers and no assumed role performs;
//! * every `Timestamp` this service serializes is an epoch-seconds number at millisecond
//!   resolution, the resolution CodePipeline's own timestamps carry, whichever operation
//!   reports it.
//!
//! Only a cross-service suite can pin the first two: the source action's work is performed
//! against Amazon S3 (the object it reads, the version that object carries and the
//! artifact store it writes), so the provider id the record must report is a fact held by
//! another service's store. CodePipeline and Amazon S3 are mounted on ONE engine here,
//! exactly as `core/router/src/lib.rs` mounts them.
//!
//! Nothing here sleeps for an execution to finish: it POLLS the reporting operations, as a
//! client does, because every step of an execution is materialised by the call that comes
//! after the clock made it due.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// CodePipeline's `X-Amz-Target` prefix, from that service's own model (service shape
/// `CodePipeline_20150709`).
const TARGET_PREFIX: &str = "CodePipeline_20150709";

/// The Region every call of this suite is signed for. One Region keeps `CreateBucket` free
/// of a `LocationConstraint`, which is not what is under test here.
const REGION: &str = "us-east-1";

/// The XML namespace an Amazon S3 request body carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The pipeline's own service role, satisfying `RoleArn`'s modelled pattern. IAM is not
/// mounted here, so no role lookup has an answer and the declarations are accepted (only a
/// definitive "no such role" refuses one).
const PIPELINE_ROLE: &str = "arn:aws:iam::123456789012:role/service-role/cp-provenance";

/// A SECOND role, for the row whose action declares a role of its own.
const ACTION_ROLE: &str = "arn:aws:iam::123456789012:role/cp-provenance-action";

/// The object key every source action of this suite reads.
const SOURCE_KEY: &str = "source.zip";

/// The name of the stage that holds each pipeline's approval, and of the approval action.
const APPROVE_STAGE: &str = "Approve";
const APPROVE_ACTION: &str = "ReleaseApproval";

/// The message the approval request carries, and the summary its reviewer answers with.
const APPROVAL_MESSAGE: &str = "release the build the source stage produced";
const APPROVAL_SUMMARY: &str = "reviewed and released";

/// How long a wait for one execution's next state may take before the suite calls it
/// stalled. Every step of an execution falls due a few hundred milliseconds after the one
/// before it, so a pipeline of a handful of actions settles well inside this; it is a
/// deliberately generous ceiling for a loaded or slow host, not an expectation — the waits
/// below look again for the CONDITION and stop as soon as it holds.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How often a wait re-reads the reporting operation it polls.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The members of this service that carry a `Timestamp`, as the model declares them
/// (`smithy_query query="shape"` on `PipelineSummary` (`created`/`updated`),
/// `PipelineMetadata` (`created`/`updated`), `GetPipelineStateOutput`
/// (`created`/`updated`), `ActionExecution` (`lastStatusChange`), `ActionExecutionDetail`
/// (`startTime`/`lastUpdateTime`), `PipelineExecutionSummary`
/// (`startTime`/`lastUpdateTime`), `TransitionState` (`lastChangedAt`),
/// `PutApprovalResultOutput` (`approvedAt`), `ActionRevision` (`created`) and
/// `ListWebhookItem` (`lastTriggered`)). Every number a response carries under one of
/// these names is a serialized timestamp, wherever in the body it sits.
const TIMESTAMP_MEMBERS: [&str; 8] = [
    "approvedAt",
    "created",
    "lastChangedAt",
    "lastStatusChange",
    "lastTriggered",
    "lastUpdateTime",
    "startTime",
    "updated",
];

/// CodePipeline and Amazon S3 over ONE shared engine, with the runtime the suite's
/// requests are driven on.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_codepipeline_codepipeline_20150709::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One raw request to the emulator's front door, answered with its status, headers and
    /// body.
    async fn send(
        &self,
        signing_name: &str,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> (u16, http::HeaderMap, Vec<u8>) {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host, Signature=abc"
                ),
            );
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(Bytes::copy_from_slice(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let response_headers = response.headers().clone();
        (status, response_headers, response.into_body().to_vec())
    }

    /// One awsJson1_1 CodePipeline operation that has to succeed.
    async fn cp_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let target = format!("{TARGET_PREFIX}.{operation}");
        let (status, _, payload) = self
            .send(
                "codepipeline",
                "POST",
                "/",
                &[
                    ("content-type", "application/x-amz-json-1.1"),
                    ("x-amz-target", &target),
                ],
                body.to_string().as_bytes(),
            )
            .await;
        let answer: Value = if payload.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&payload).unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: {operation} answered a body that is not JSON ({error}): {}",
                    String::from_utf8_lossy(&payload)
                )
            })
        };
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must answer 200, got {status} with {answer}"
        );
        answer
    }

    /// Create `bucket`, and enable versioning on it when `versioned`.
    async fn bucket(&self, case: &str, bucket: &str, versioned: bool) {
        let (status, _, body) = self
            .send("s3", "PUT", &format!("/{bucket}"), &[], b"")
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: CreateBucket {bucket} must answer 200, got {status} with {:?}",
            String::from_utf8_lossy(&body)
        );
        if !versioned {
            return;
        }
        let payload = format!(
            "<VersioningConfiguration xmlns=\"{S3_XMLNS}\"><Status>Enabled</Status>\
             </VersioningConfiguration>"
        );
        let (status, _, body) = self
            .send(
                "s3",
                "PUT",
                &format!("/{bucket}?versioning"),
                &[("content-type", "application/xml")],
                payload.as_bytes(),
            )
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: {bucket} must take versioning, got {status} with {:?}",
            String::from_utf8_lossy(&body)
        );
    }

    /// Write the source object and answer `(version id, entity tag)` as Amazon S3 reported
    /// them — the two forms a revision of one object is named by.
    async fn put_source(&self, case: &str, bucket: &str, bytes: &[u8]) -> (Option<String>, String) {
        let (status, headers, body) = self
            .send("s3", "PUT", &format!("/{bucket}/{SOURCE_KEY}"), &[], bytes)
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: PutObject s3://{bucket}/{SOURCE_KEY} must answer 200, got {status} \
             with {:?}",
            String::from_utf8_lossy(&body)
        );
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.trim_matches('"').to_string())
        };
        let etag = header("etag").unwrap_or_else(|| {
            panic!("case {case:?}: the written object must report an entity tag")
        });
        (header("x-amz-version-id"), etag)
    }

    /// Every `ActionExecutionDetail` of `pipeline`, keyed by action name.
    async fn action_executions(&self, case: &str, pipeline: &str) -> Vec<Value> {
        let answer = self
            .cp_ok(case, "ListActionExecutions", &json!({"pipelineName": pipeline}))
            .await;
        answer["actionExecutionDetails"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The one record `ListActionExecutions` reports for `action`.
    async fn record_of(&self, case: &str, pipeline: &str, action: &str) -> Value {
        let details = self.action_executions(case, pipeline).await;
        let found: Vec<&Value> = details
            .iter()
            .filter(|detail| detail["actionName"] == json!(action))
            .collect();
        assert_eq!(
            found.len(),
            1,
            "case {case:?}: {action} has exactly one action execution, got {details:?}"
        );
        found[0].clone()
    }

    /// The `ActionState` `GetPipelineState` reports for `action` of `stage`.
    async fn action_state(&self, case: &str, pipeline: &str, stage: &str, action: &str) -> Value {
        let state = self
            .cp_ok(case, "GetPipelineState", &json!({"name": pipeline}))
            .await;
        state["stageStates"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|reported| reported["stageName"] == json!(stage))
            .filter_map(|reported| reported["actionStates"].as_array())
            .flatten()
            .find(|reported| reported["actionName"] == json!(action))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: {stage}/{action} must be reported in {state}"))
    }
}

/// An S3 source action reading the object this suite writes, with `role` as its own
/// `roleArn` when it declares one.
fn source_action(name: &str, bucket: &str, role: Option<&str>) -> Value {
    let mut action = json!({
        "name": name,
        "actionTypeId": {"category": "Source", "owner": "AWS", "provider": "S3", "version": "1"},
        "configuration": {
            "S3Bucket": bucket,
            "S3ObjectKey": SOURCE_KEY,
            "PollForSourceChanges": "false",
        },
        "outputArtifacts": [{"name": "SourceArtifact"}],
        "runOrder": 1,
    });
    if let Some(role) = role {
        action["roleArn"] = json!(role);
    }
    action
}

/// The manual-approval action that holds the second stage until a reviewer answers it.
fn approval_action() -> Value {
    json!({
        "name": APPROVE_ACTION,
        "actionTypeId": {"category": "Approval", "owner": "AWS", "provider": "Manual", "version": "1"},
        "configuration": {"CustomData": APPROVAL_MESSAGE},
        "runOrder": 1,
    })
}

/// A pipeline whose source stage reads `bucket` and whose second stage waits for an
/// approval.
fn declaration(name: &str, bucket: &str, source_role: Option<&str>) -> Value {
    json!({
        "name": name,
        "roleArn": PIPELINE_ROLE,
        "artifactStore": {"type": "S3", "location": bucket},
        "stages": [
            {"name": "Source", "actions": [source_action("Src", bucket, source_role)]},
            {"name": APPROVE_STAGE, "actions": [approval_action()]},
        ],
    })
}

/// The serialized form of one timestamp member: its fractional digits with trailing zeros
/// dropped, as the wire carries them. A number written with no fraction at all has none.
fn fractional_digits(number: &Value) -> String {
    let serialized = number.to_string();
    match serialized.split_once('.') {
        Some((_whole, fraction)) => fraction.trim_end_matches('0').to_string(),
        None => String::new(),
    }
}

/// Assert that every timestamp member anywhere in `body` is serialized at millisecond
/// resolution, and answer how many were found, so a response carrying none cannot pass the
/// assertion vacuously.
pub(crate) fn assert_millisecond_timestamps(case: &str, operation: &str, body: &Value) -> usize {
    match body {
        Value::Object(members) => members
            .iter()
            .map(|(name, value)| {
                let mut found = assert_millisecond_timestamps(case, operation, value);
                if TIMESTAMP_MEMBERS.contains(&name.as_str()) && value.is_number() {
                    found += 1;
                    let digits = fractional_digits(value);
                    assert!(
                        digits.len() <= 3,
                        "case {case:?}: {operation} serializes {name} at millisecond \
                         resolution, got {value} with {} fractional digits",
                        digits.len()
                    );
                }
                found
            })
            .sum(),
        Value::Array(items) => items
            .iter()
            .map(|item| assert_millisecond_timestamps(case, operation, item))
            .sum(),
        _ => 0,
    }
}

/// One row of the table: a pipeline whose source bucket does or does not hold versions, and
/// whose source action does or does not declare a role of its own.
struct Row {
    case: &'static str,
    pipeline: &'static str,
    bucket: &'static str,
    versioned: bool,
    source_role: Option<&'static str>,
}

/// The rows differ along what the rules generalise over: what the source provider names the
/// revision it delivered by (an object version, an entity tag) and which role performs the
/// source action (the pipeline's, the action's own).
const ROWS: [Row; 2] = [
    Row {
        case: "a versioned source object and the pipeline's own role",
        pipeline: "cp-provenance-versioned",
        bucket: "cp-provenance-versioned",
        versioned: true,
        source_role: None,
    },
    Row {
        case: "an unversioned source object and an action declaring its own role",
        pipeline: "cp-provenance-plain",
        bucket: "cp-provenance-plain",
        versioned: false,
        source_role: Some(ACTION_ROLE),
    },
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_action_execution_records_its_providers_external_id_and_only_a_performing_role() {
    let emulator = Emulator::new();

    for row in &ROWS {
        let case = row.case;
        emulator.bucket(case, row.bucket, row.versioned).await;
        let (version, etag) = emulator
            .put_source(case, row.bucket, case.as_bytes())
            .await;
        assert_eq!(
            version.is_some(),
            row.versioned,
            "case {case:?}: a versioned bucket's write reports a version id and an \
             unversioned one does not, got {version:?}"
        );
        emulator
            .cp_ok(
                case,
                "CreatePipeline",
                &json!({"pipeline": declaration(row.pipeline, row.bucket, row.source_role)}),
            )
            .await;

        // The approval publishes the token its reviewer answers it with once the source
        // action has delivered its artifact and the stage has been entered. The wait is for
        // that CONDITION: the token is looked for again until it is there or the deadline
        // passes.
        let deadline = Instant::now() + SETTLE_TIMEOUT;
        let token = loop {
            let action = emulator
                .action_state(case, row.pipeline, APPROVE_STAGE, APPROVE_ACTION)
                .await;
            if let Some(token) = action["latestExecution"]["token"].as_str() {
                break token.to_string();
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the approval must publish its token within \
                 {SETTLE_TIMEOUT:?}; last observed {action}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        let approved = emulator
            .cp_ok(
                case,
                "PutApprovalResult",
                &json!({
                    "pipelineName": row.pipeline,
                    "stageName": APPROVE_STAGE,
                    "actionName": APPROVE_ACTION,
                    "result": {"status": "Approved", "summary": APPROVAL_SUMMARY},
                    "token": token,
                }),
            )
            .await;

        // RULE 1, the source action: the provider's external id for the run is the revision
        // it delivered — the value the action itself publishes as its output variable, which
        // is the object's version where the bucket holds versions and its entity tag where
        // it does not.
        let source = loop {
            let record = emulator.record_of(case, row.pipeline, "Src").await;
            if record["output"]["executionResult"]["externalExecutionId"].is_string() {
                break record;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the source run must report its provider's external id within \
                 {SETTLE_TIMEOUT:?}; last observed {record}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        let published = source["output"]["outputVariables"]
            [if row.versioned { "VersionId" } else { "ETag" }]
            .as_str()
            .map(str::to_string);
        let delivered = version.clone().unwrap_or_else(|| etag.clone());
        assert_eq!(
            published.as_deref(),
            Some(delivered.as_str()),
            "case {case:?}: the source action publishes the revision it delivered, got {source}"
        );
        assert_eq!(
            source["output"]["executionResult"]["externalExecutionId"].as_str(),
            Some(delivered.as_str()),
            "case {case:?}: the source run reports the provider's id for the revision it \
             delivered, got {source}"
        );
        // And the state projection agrees with the history projection about it.
        let source_state = emulator
            .action_state(case, row.pipeline, "Source", "Src")
            .await;
        assert_eq!(
            source_state["latestExecution"]["externalExecutionId"].as_str(),
            Some(delivered.as_str()),
            "case {case:?}: both projections report one external id, got {source_state}"
        );

        // RULE 1, the approval: the provider's record of the decision IS the action
        // execution, so its external id is that execution's own id, reported beside the
        // provider's summary.
        let approval = loop {
            let record = emulator
                .record_of(case, row.pipeline, APPROVE_ACTION)
                .await;
            if record["status"] == json!("Succeeded") {
                break record;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the answered approval must be reported Succeeded within \
                 {SETTLE_TIMEOUT:?}; last observed {record}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        let approval_id = approval["actionExecutionId"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: the approval run is reported under an id, got {approval}")
        });
        assert_eq!(
            approval["output"]["executionResult"]["externalExecutionId"].as_str(),
            Some(approval_id),
            "case {case:?}: the approval's external id is the decision's own id, got {approval}"
        );
        assert_eq!(
            approval["output"]["executionResult"]["externalExecutionSummary"].as_str(),
            Some(APPROVAL_SUMMARY),
            "case {case:?}: the approval's result carries the reviewer's summary, got {approval}"
        );

        // RULE 2: the role reported is the role that PERFORMS the action — the action's own
        // when it declares one, else the pipeline's — and an approval reports none.
        assert_eq!(
            source["input"]["roleArn"].as_str(),
            Some(row.source_role.unwrap_or(PIPELINE_ROLE)),
            "case {case:?}: the source action reports the role that performs it, got {source}"
        );
        assert_eq!(
            approval["input"].get("roleArn"),
            None,
            "case {case:?}: a manual approval is answered by a principal, so its input names \
             no service role, got {approval}"
        );

        // RULE 3: every timestamp of every reporting operation is serialized at millisecond
        // resolution.
        let mut found = assert_millisecond_timestamps(case, "PutApprovalResult", &approved);
        for (operation, request) in [
            ("ListPipelines", json!({})),
            ("GetPipeline", json!({"name": row.pipeline})),
            ("GetPipelineState", json!({"name": row.pipeline})),
            ("ListActionExecutions", json!({"pipelineName": row.pipeline})),
            (
                "ListPipelineExecutions",
                json!({"pipelineName": row.pipeline}),
            ),
        ] {
            let answer = emulator.cp_ok(case, operation, &request).await;
            let reported = assert_millisecond_timestamps(case, operation, &answer);
            assert!(
                reported > 0,
                "case {case:?}: {operation} reports at least one timestamp, got {answer}"
            );
            found += reported;
        }
        assert!(
            found > 5,
            "case {case:?}: the reporting operations together report a timestamp per \
             resource and run, got {found}"
        );

        // The instants themselves stay ordered through that resolution: a pipeline is
        // created no later than it was last updated.
        let listed = emulator.cp_ok(case, "ListPipelines", &json!({})).await;
        let summary = listed["pipelines"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|summary| summary["name"] == json!(row.pipeline))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the listing reports {}", row.pipeline));
        let instant = |member: &str| summary[member].as_f64().unwrap_or_default();
        assert!(
            instant("created") > 0.0 && instant("created") <= instant("updated"),
            "case {case:?}: the summary's instants are real and ordered, got {summary}"
        );
    }
}
