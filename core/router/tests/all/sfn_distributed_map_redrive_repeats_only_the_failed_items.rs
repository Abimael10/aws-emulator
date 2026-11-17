// SPDX-License-Identifier: CC0-1.0

//! What a Distributed `Map` state leaves BEHIND for a client to find, and what a redrive
//! of its parent does in the outside world, driven across the two crates the question
//! spans (Step Functions runs the map, S3 holds the dataset it reads and the markers its
//! items write), mounted on ONE engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULE: an execution that enters a `Map` state in `DISTRIBUTED` mode materialises
//! exactly one Map Run for that state — the handle `ListMapRuns` serves for the parent
//! execution ARN and `DescribeMapRun` and `ListExecutions` answer for — and runs one
//! child workflow execution per item of the state's dataset; redriving the parent re-runs
//! exactly the items whose child execution did not succeed, under the Map Run the state
//! already has, so the service calls a succeeded item's inner workflow made are NOT made
//! a second time while a re-run item's are made again.
//!
//! The work is counted where it lands rather than inferred from the run's own records:
//! each item's inner workflow writes one S3 object named after the item and the child
//! execution that ran it, so the objects a prefix holds after the redrive are one per
//! attempt that really ran that item — one for every item the first attempt settled, and
//! a second one for each item the redrive ran again.
//!
//! The rows differ along what the rule generalises over: how many items the dataset holds
//! (one, a few, nine), WHICH of them fail (the first, the last, several scattered
//! through the run, all of them) and how many do (none, one, three, every one). The row
//! where none fails is the boundary the rule meets from the other side: nothing is owed a
//! second attempt, the execution is not redrivable at all, and every item's work stays
//! done exactly once.
//!
//! The item ids, the marker prefix and the error name are this suite's own request
//! values; what is asserted about them is only what the rule says.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for.
const REGION: &str = "us-west-2";

/// The role a state machine is created under. Nothing here is about authorization, so it
/// is one ARN inside the modelled `Arn` length.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/StepFunctionsRole";

/// Step Functions' wire facts, from its model's own `service_info`: the service shape the
/// `X-Amz-Target` prefix is, its `sigv4Name`, and the `awsJson` version its protocol
/// declares.
const SFN_TARGET_PREFIX: &str = "AWSStepFunctions";
const SFN_SIGNING_NAME: &str = "states";
const SFN_JSON_VERSION: &str = "1.0";

/// S3's signing name, which is what routes a request to it.
const S3_SIGNING_NAME: &str = "s3";

/// The bucket holding every dataset read below and every marker written below.
const BUCKET: &str = "distributed-map-work";

/// How long a poll waits for the condition it is waiting on — an execution leaving
/// `RUNNING`. Generous: every loop stops as soon as its condition holds, so this bounds
/// only a failure.
const PATIENCE: Duration = Duration::from_secs(120);

/// How long a poll waits between looks.
const GLANCE: Duration = Duration::from_millis(25);

/// Step Functions and S3 over ONE engine, so the dataset a `Map` state's `ItemReader`
/// reads and the markers its items write are the objects S3's own handlers stored.
struct Chain {
    router: MultiServiceRouter,
}

impl Chain {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// One Step Functions call, as its status and its body.
    async fn call(&self, operation: &str, body: &Value) -> (u16, Value) {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{SFN_SIGNING_NAME}/\
             aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{SFN_SIGNING_NAME}.{REGION}.amazonaws.com"))
            .header(
                "content-type",
                format!("application/x-amz-json-{SFN_JSON_VERSION}"),
            )
            .header("x-amz-target", format!("{SFN_TARGET_PREFIX}.{operation}"))
            .header("authorization", authorization)
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One Step Functions call that must answer 200, as its body.
    async fn sfn(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "case {operation}: expected 200 from Step Functions, got {status} with {parsed:?}"
        );
        parsed
    }

    /// One S3 request, PATH-STYLE against the Region's own endpoint — the way a client
    /// with no bucket in its host addresses S3 — as its status and its body.
    async fn s3(&self, method: &str, uri: &str, body: &[u8]) -> (u16, String) {
        let host = format!("{S3_SIGNING_NAME}.{REGION}.amazonaws.com");
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{S3_SIGNING_NAME}/\
             aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
        );
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
            .header("authorization", authorization)
            .body(Bytes::copy_from_slice(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (
            status,
            String::from_utf8_lossy(&response.into_body()).to_string(),
        )
    }

    /// The one bucket this suite reads and writes, in this Region.
    async fn bucket(&self) {
        let configuration = format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
             </CreateBucketConfiguration>"
        );
        let (status, body) = self
            .s3("PUT", &format!("/{BUCKET}"), configuration.as_bytes())
            .await;
        assert_eq!(
            status, 200,
            "the bucket {BUCKET:?} must be created first, got {status} with {body}"
        );
    }

    /// One object, stored through S3's own `PutObject`.
    async fn object(&self, key: &str, body: &str) {
        let (status, answered) = self
            .s3("PUT", &format!("/{BUCKET}/{key}"), body.as_bytes())
            .await;
        assert_eq!(
            status, 200,
            "the object {key:?} must be stored first, got {status} with {answered}"
        );
    }

    /// Every object key S3 holds under `prefix`, read back through its own `ListObjectsV2`.
    async fn keys_under(&self, prefix: &str) -> Vec<String> {
        let (status, body) = self
            .s3(
                "GET",
                &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
                &[],
            )
            .await;
        assert_eq!(
            status, 200,
            "expected a listing of {prefix:?}, got {status} with {body}"
        );
        let mut keys = elements(&body, "Key");
        keys.sort();
        keys
    }

    /// Create a state machine over `definition` and return its ARN.
    async fn machine(&self, name: &str, definition: &Value) -> String {
        let created = self
            .sfn(
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": definition.to_string(),
                    "roleArn": ROLE_ARN,
                }),
            )
            .await;
        string(&created, "stateMachineArn")
    }

    /// Poll one execution until it has left `RUNNING`, and report how it settled.
    async fn settled(&self, execution_arn: &str, case: &str) -> Value {
        let began = Instant::now();
        loop {
            let described = self
                .sfn("DescribeExecution", &json!({ "executionArn": execution_arn }))
                .await;
            if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
                return described;
            }
            assert!(
                began.elapsed() < PATIENCE,
                "case {case:?}: expected a terminal status within {PATIENCE:?}, got {described:?}"
            );
            ::tokio::time::sleep(GLANCE).await;
        }
    }

    /// The Map Runs `execution_arn` started, as `ListMapRuns` serves them.
    async fn map_runs(&self, execution_arn: &str) -> Vec<Value> {
        let listed = self
            .sfn("ListMapRuns", &json!({ "executionArn": execution_arn }))
            .await;
        listed
            .get("mapRuns")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// The child workflow executions of one Map Run, as `DescribeExecution` reports each
    /// of them, grouped by the `id` member of the item document it was handed.
    async fn children(&self, map_run_arn: &str) -> BTreeMap<i64, Vec<Value>> {
        let listed = self
            .sfn(
                "ListExecutions",
                &json!({ "mapRunArn": map_run_arn, "maxResults": 1000 }),
            )
            .await;
        let mut children: BTreeMap<i64, Vec<Value>> = BTreeMap::new();
        for child in listed
            .get("executions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let arn = string(&child, "executionArn");
            let described = self
                .sfn("DescribeExecution", &json!({ "executionArn": arn }))
                .await;
            let input: Value = described
                .get("input")
                .and_then(Value::as_str)
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or(Value::Null);
            let id = input
                .get("id")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| panic!("expected an item id in the child input {input:?}"));
            children.entry(id).or_default().push(described);
        }
        children
    }
}

/// The text a member of a response carries.
fn string(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a string {name} in {body}"))
        .to_string()
}

/// The text of every `<tag>` element of an XML answer, in the order it carries them.
fn elements(xml: &str, tag: &str) -> Vec<String> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut values = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        rest = &rest[start + open.len()..];
        let Some(end) = rest.find(&close) else { break };
        values.push(rest[..end].to_string());
        rest = &rest[end + close.len()..];
    }
    values
}

/// `count` item documents, every one of which succeeds except the indices in `failing`,
/// each naming the `run` whose markers it writes under.
fn items(count: usize, failing: &[usize], run: &str) -> Value {
    Value::Array(
        (0..count)
            .map(|index| json!({ "id": index, "ok": !failing.contains(&index), "run": run }))
            .collect(),
    )
}

/// The inner workflow every item runs: it WRITES one object named after the item and the
/// child execution running it — the work whose repetition is counted — and then succeeds
/// or fails as the item itself says.
fn marking_processor() -> Value {
    json!({
        "ProcessorConfig": { "Mode": "DISTRIBUTED", "ExecutionType": "STANDARD" },
        "StartAt": "Stamp",
        "States": {
            // The name of the child execution running this item, taken off the context
            // object, is what tells one attempt's write from another's.
            "Stamp": {
                "Type": "Pass",
                "Parameters": {
                    "id.$": "$.id",
                    "ok.$": "$.ok",
                    "run.$": "$.run",
                    "attempt.$": "$$.Execution.Name",
                },
                "Next": "Mark",
            },
            "Mark": {
                "Type": "Task",
                "Resource": "arn:aws:states:::aws-sdk:s3:putObject",
                "Parameters": {
                    "Bucket": BUCKET,
                    "Key.$": "States.Format('markers/{}/{}/{}', $.run, $.id, $.attempt)",
                    "Body": "ran",
                },
                "ResultPath": "$.wrote",
                "Next": "Decide",
            },
            "Decide": {
                "Type": "Choice",
                "Choices": [{ "Variable": "$.ok", "BooleanEquals": true, "Next": "Keep" }],
                "Default": "Boom",
            },
            "Keep": { "Type": "Pass", "Parameters": { "id.$": "$.id", "done": true }, "End": true },
            "Boom": { "Type": "Fail", "Error": "ItemRejected", "Cause": "the item said so" },
        },
    })
}

/// A state machine whose one Distributed `Map` state reads its items from the S3 object
/// `dataset_key` holds.
fn mapping(dataset_key: &str) -> Value {
    json!({
        "StartAt": "Fan",
        "States": {
            "Fan": {
                "Type": "Map",
                "ItemReader": {
                    "Resource": "arn:aws:states:::s3:getObject",
                    "ReaderConfig": { "InputType": "JSON" },
                    "Parameters": { "Bucket": BUCKET, "Key": dataset_key },
                },
                "ItemProcessor": marking_processor(),
                "End": true,
            },
        },
    })
}

/// One row: how many items the dataset holds and which of them fail.
struct Row {
    case: &'static str,
    count: usize,
    failing: &'static [usize],
}

/// A Distributed Map state is one Map Run `ListMapRuns` serves for its parent execution,
/// and a redrive of that parent runs again exactly the items that did not succeed — which
/// is counted in the work their inner workflows really did.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_redrive_repeats_the_work_of_only_the_items_that_did_not_succeed() {
    let rows: &[Row] = &[
        Row {
            case: "three of nine items fail",
            count: 9,
            failing: &[2, 4, 7],
        },
        Row {
            case: "the first of three items fails",
            count: 3,
            failing: &[0],
        },
        Row {
            case: "the last of four items fails",
            count: 4,
            failing: &[3],
        },
        Row {
            case: "every item of two fails",
            count: 2,
            failing: &[0, 1],
        },
        Row {
            case: "the one item fails",
            count: 1,
            failing: &[0],
        },
        Row {
            case: "no item fails",
            count: 3,
            failing: &[],
        },
    ];

    let chain = Chain::new();
    chain.bucket().await;
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let run = format!("run-{index}");
        let dataset_key = format!("datasets/{run}.json");
        let dataset = items(row.count, row.failing, &run).to_string();
        chain.object(&dataset_key, &dataset).await;
        let machine = chain
            .machine(&format!("mapped-{index}"), &mapping(&dataset_key))
            .await;
        let started = chain
            .sfn(
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": "only", "input": "{}" }),
            )
            .await;
        let execution = string(&started, "executionArn");
        let described = chain.settled(&execution, case).await;
        let expected_status = match row.failing.is_empty() {
            true => "SUCCEEDED",
            false => "FAILED",
        };
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some(expected_status),
            "case {case:?}: expected {expected_status}, got {described:?}"
        );

        // The Map Run the state materialised is the handle a client holds: exactly one of
        // them for this execution, whatever the items did.
        let listed = chain.map_runs(&execution).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: one Distributed Map state is one Map Run, got {listed:?}"
        );
        let map_run_arn = string(&listed[0], "mapRunArn");
        assert_eq!(
            listed[0].get("executionArn").and_then(Value::as_str),
            Some(execution.as_str()),
            "case {case:?}: the listed Map Run names the execution that started it, got \
             {listed:?}"
        );
        let described_run = chain
            .sfn("DescribeMapRun", &json!({ "mapRunArn": map_run_arn }))
            .await;
        let counts = |member: &str| {
            described_run
                .pointer(&format!("/itemCounts/{member}"))
                .and_then(Value::as_i64)
        };
        let total = i64::try_from(row.count).unwrap_or(i64::MAX);
        let failed = i64::try_from(row.failing.len()).unwrap_or(i64::MAX);
        assert_eq!(
            (counts("total"), counts("succeeded"), counts("failed")),
            (Some(total), Some(total - failed), Some(failed)),
            "case {case:?}: the Map Run counts its items as they turned out, got \
             {described_run:?}"
        );

        // Every item ran once, so every item's inner workflow wrote once.
        for id in 0..row.count {
            let prefix = format!("markers/{run}/{id}/");
            let wrote = chain.keys_under(&prefix).await;
            assert_eq!(
                wrote.len(),
                1,
                "case {case:?}: item {id} ran once on the first attempt, so it wrote once, \
                 got {wrote:?}"
            );
        }
        let before = chain.children(&map_run_arn).await;
        assert_eq!(
            before.len(),
            row.count,
            "case {case:?}: every item ran as a child workflow execution, got {before:?}"
        );

        // An execution with nothing left unsuccessful is not redrivable at all, so no
        // item's work is done a second time.
        if row.failing.is_empty() {
            let (status, body) = chain
                .call("RedriveExecution", &json!({ "executionArn": execution }))
                .await;
            let code = body
                .get("__type")
                .or_else(|| body.get("code"))
                .and_then(Value::as_str)
                .map(|name| name.rsplit(['#', '/']).next().unwrap_or(name).to_string())
                .unwrap_or_default();
            assert_eq!(
                (status, code.as_str()),
                (400, "ExecutionNotRedrivable"),
                "case {case:?}: a run that left nothing unsuccessful is not redrivable, got \
                 {status} with {body:?}"
            );
            for id in 0..row.count {
                let prefix = format!("markers/{run}/{id}/");
                let wrote = chain.keys_under(&prefix).await;
                assert_eq!(
                    wrote.len(),
                    1,
                    "case {case:?}: item {id} was never run again, so its work stands once, \
                     got {wrote:?}"
                );
            }
            continue;
        }

        chain
            .sfn("RedriveExecution", &json!({ "executionArn": execution }))
            .await;
        let redriven = chain.settled(&execution, case).await;
        assert_eq!(
            redriven.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: the items that failed fail again, so the second attempt fails \
             too, got {redriven:?}"
        );

        // The redrive re-ran the same Map state, so the Map Run is the one the client
        // already holds, under the ARN it already had, and it counts the redrive.
        let again = chain.map_runs(&execution).await;
        assert_eq!(
            again
                .iter()
                .map(|listed| string(listed, "mapRunArn"))
                .collect::<Vec<String>>(),
            vec![map_run_arn.clone()],
            "case {case:?}: the redriven state is the same Map Run, got {again:?}"
        );
        let described_run = chain
            .sfn("DescribeMapRun", &json!({ "mapRunArn": map_run_arn }))
            .await;
        assert_eq!(
            described_run.get("redriveCount").and_then(Value::as_i64),
            Some(1),
            "case {case:?}: one redrive is counted once, got {described_run:?}"
        );

        // The work: an item that had succeeded wrote once in all, and one the redrive ran
        // again wrote a second time, under the name of the child execution that re-ran it.
        let after = chain.children(&map_run_arn).await;
        for id in 0..row.count {
            let re_ran = row.failing.contains(&id);
            let prefix = format!("markers/{run}/{id}/");
            let wrote = chain.keys_under(&prefix).await;
            let expected = match re_ran {
                true => 2,
                false => 1,
            };
            assert_eq!(
                wrote.len(),
                expected,
                "case {case:?}: item {id} ran {expected} time(s) in all, so it wrote that \
                 many markers, got {wrote:?}"
            );
            // Each marker is named after a child execution that really ran this item, and
            // the child the run reports for it now is among them.
            let latest = after
                .get(&i64::try_from(id).unwrap_or(i64::MAX))
                .and_then(|children| children.last())
                .unwrap_or_else(|| panic!("case {case:?}: expected a child for item {id}"));
            let name = string(latest, "name");
            assert!(
                wrote.iter().any(|key| key.ends_with(&name)),
                "case {case:?}: item {id}'s markers are named after the children that ran \
                 it, so {name:?} is among {wrote:?}"
            );
        }

        // And the records agree with the work: an item kept the execution it ran as
        // exactly if it had succeeded.
        for (id, children) in &before {
            let was = children
                .last()
                .unwrap_or_else(|| panic!("case {case:?}: expected a child for item {id}"));
            let now = after
                .get(id)
                .and_then(|children| children.last())
                .unwrap_or_else(|| panic!("case {case:?}: expected a child for item {id} now"));
            let kept = was.get("executionArn") == now.get("executionArn");
            assert_eq!(
                kept,
                was.get("status").and_then(Value::as_str) == Some("SUCCEEDED"),
                "case {case:?}: item {id} kept its own execution exactly if it had \
                 succeeded: was {was:?}, now {now:?}"
            );
        }
    }
}
