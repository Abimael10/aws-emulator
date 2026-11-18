// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a `Task` state whose `Resource` names an S3 operation through
//! the AWS SDK integration family, driven over both services' own wire protocols
//! through the production `MultiServiceRouter` with Step Functions, S3 and IAM mounted
//! on ONE engine.
//!
//! The rule under test: `arn:aws:states:::aws-sdk:s3:<action>` is CALLED on S3's own
//! handler, and the state's result is that operation's own output — an object's content
//! arrives as the TEXT its bytes are, under the `Body` member the operation binds its
//! payload to, so a state reads a JSON object with `States.StringToJson` exactly as it
//! does on the real service — while a refusal is reported as the error that service's
//! own client raises. An operation whose answer this emulator does not read as that
//! operation's output members is not run, and nothing is synthesised for it.
//!
//! The call is made wherever the `Task` state SITS, which the last table below is
//! about: in the child branch of a DISTRIBUTED `Map` state, each item's own call
//! decides that item's child execution, so a map over items some of whose objects are
//! not there is a Map Run with a real succeeded/failed split weighed against the
//! state's tolerated-failure threshold, and a redrive of its parent re-runs exactly the
//! items whose own call had failed.
//!
//! The rows span what the rule generalises over: what the object holds (a JSON
//! document, plain text, an empty object), which operation the `Resource` names (a
//! read, a write, one that is not read as output members), whether the object is
//! there at all and — for the Map table — how many items there are, which of them
//! address an object nobody stored, and how the threshold is stated.
//!
//! The harness is `crate::map_result_writer`'s: `tests/all.rs` is this binary's crate
//! root, so this suite drives the same three services through it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::{
    RUNTIME_ERROR, TOLERATED_FAILURE_ERROR,
};

use crate::map_result_writer::{every_action, Chain, BUCKET};

/// A definition whose one `Task` state makes `resource`'s call with `parameters` and
/// ends with whatever it answered.
fn calling(resource: &str, parameters: Value) -> Value {
    json!({
        "StartAt": "Call",
        "States": {
            "Call": {
                "Type": "Task",
                "Resource": resource,
                "Parameters": parameters,
                "End": true,
            },
        },
    })
}

/// One row of the read table: what the object holds, and what the state's result must
/// carry under `Body`.
type ReadRow = (&'static str, &'static str, &'static str);

/// An AWS SDK `s3:getObject` task answers the object's own bytes as the TEXT of its
/// `Body` member.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_sdk_s3_read_answers_the_objects_bytes_as_text() {
    // (case, what the object holds, the text the result's `Body` must carry)
    let rows: &[ReadRow] = &[
        (
            "a JSON document",
            "{\"id\": 7, \"ok\": true}",
            "{\"id\": 7, \"ok\": true}",
        ),
        ("a JSON array", "[1, 2, 3]", "[1, 2, 3]"),
        ("plain text", "not json at all", "not json at all"),
        (
            "a document with its own spacing kept",
            "{\n  \"id\": 7\n}\n",
            "{\n  \"id\": 7\n}\n",
        ),
    ];

    let chain = Chain::new();
    chain.bucket(BUCKET).await;
    let role_arn = chain.role("SdkReadRole", &every_action()).await;
    for (index, (case, held, expected)) in rows.iter().enumerate() {
        let key = format!("sdk/read-{index}");
        chain.object(&key, held).await;
        let definition = calling(
            "arn:aws:states:::aws-sdk:s3:getObject",
            json!({ "Bucket": BUCKET, "Key": key }),
        );
        let machine = chain
            .machine(&format!("sdk-read-{index}"), &definition, &role_arn)
            .await;
        let (_execution, described) = chain.ran(&machine, &json!({}), case).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected SUCCEEDED, got {described:?}"
        );
        let output: Value = described
            .get("output")
            .and_then(Value::as_str)
            .map(|text| serde_json::from_str(text).expect("the output is JSON"))
            .unwrap_or_else(|| panic!("case {case:?}: expected an output in {described:?}"));
        assert_eq!(
            output.get("Body").and_then(Value::as_str),
            Some(*expected),
            "case {case:?}: the result carries the object's bytes as text, got {output:?}"
        );
        assert!(
            output.get("ETag").and_then(Value::as_str).is_some(),
            "case {case:?}: the result carries the output members the answer's headers \
             carried, got {output:?}"
        );
    }
}

/// A state that reads a JSON object can parse it with `States.StringToJson`, which is
/// what the `Body` member being a STRING is for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_object_parses_with_string_to_json() {
    let chain = Chain::new();
    chain.bucket(BUCKET).await;
    let role_arn = chain.role("SdkParseRole", &every_action()).await;
    chain
        .object("sdk/parsed.json", "{\"id\": 7, \"ok\": true}")
        .await;
    let definition = json!({
        "StartAt": "Read",
        "States": {
            "Read": {
                "Type": "Task",
                "Resource": "arn:aws:states:::aws-sdk:s3:getObject",
                "Parameters": { "Bucket": BUCKET, "Key": "sdk/parsed.json" },
                "ResultSelector": { "body.$": "$.Body" },
                "ResultPath": "$.fetched",
                "Next": "Parse",
            },
            "Parse": {
                "Type": "Pass",
                "Parameters": { "item.$": "States.StringToJson($.fetched.body)" },
                "End": true,
            },
        },
    });
    let machine = chain.machine("sdk-parse", &definition, &role_arn).await;
    let (_execution, described) = chain
        .ran(&machine, &json!({}), "a JSON object read and parsed")
        .await;
    let output: Value = described
        .get("output")
        .and_then(Value::as_str)
        .map(|text| serde_json::from_str(text).expect("the output is JSON"))
        .unwrap_or_else(|| panic!("expected an output in {described:?}"));
    assert_eq!(
        output,
        json!({ "item": { "id": 7, "ok": true } }),
        "the object's text parses to the document it holds, got {output:?}"
    );
}

/// An AWS SDK `s3:putObject` task really stores the object, and the state's result
/// carries the output members the answer carried.
///
/// What the state wrote for `Body` is a JSON VALUE of the document the state built, so
/// the object holds that value's own TEXT: a string is stored as the JSON string
/// literal it is in that document, and a document as its own compact text.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_sdk_s3_write_stores_the_object() {
    // (case, the body the task writes, the text the object then holds)
    let rows: &[(&str, Value, &str)] = &[
        ("text", json!("just text"), "\"just text\""),
        ("a document", json!({ "id": 7 }), "{\"id\":7}"),
        ("an array", json!([1, 2]), "[1,2]"),
        ("a number", json!(7), "7"),
    ];

    let chain = Chain::new();
    chain.bucket(BUCKET).await;
    let role_arn = chain.role("SdkWriteRole", &every_action()).await;
    for (index, (case, body, expected)) in rows.iter().enumerate() {
        let key = format!("sdk/written-{index}");
        let definition = calling(
            "arn:aws:states:::aws-sdk:s3:putObject",
            json!({ "Bucket": BUCKET, "Key": key, "Body": body }),
        );
        let machine = chain
            .machine(&format!("sdk-write-{index}"), &definition, &role_arn)
            .await;
        let (_execution, described) = chain.ran(&machine, &json!({}), case).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected SUCCEEDED, got {described:?}"
        );
        assert_eq!(
            chain.stored(&key).await.as_deref(),
            Some(*expected),
            "case {case:?}: the object S3 holds is the one the task wrote"
        );
    }
}

/// What an S3 call this emulator does not serve as a `Task` state's work reports.
///
/// The rows differ along why: an object that is not there, which S3 itself refuses, and
/// an operation whose restXml answer is not read as that operation's output members,
/// which is not run at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_s3_call_that_cannot_be_served_says_so() {
    // (case, the Resource, its parameters, the error the state must report)
    let rows: &[(&str, &str, Value, &str)] = &[
        (
            "an object that is not there",
            "arn:aws:states:::aws-sdk:s3:getObject",
            json!({ "Bucket": BUCKET, "Key": "sdk/absent" }),
            "S3.NoSuchKeyException",
        ),
        (
            "a listing, whose answer is not read as output members",
            "arn:aws:states:::aws-sdk:s3:listObjectsV2",
            json!({ "Bucket": BUCKET }),
            RUNTIME_ERROR,
        ),
        (
            "the optimized family, which S3 publishes no integration for",
            "arn:aws:states:::s3:getObject",
            json!({ "Bucket": BUCKET, "Key": "sdk/absent" }),
            RUNTIME_ERROR,
        ),
    ];

    let chain = Chain::new();
    chain.bucket(BUCKET).await;
    let role_arn = chain.role("SdkRefusalRole", &every_action()).await;
    for (index, (case, resource, parameters, expected)) in rows.iter().enumerate() {
        let definition = calling(resource, parameters.clone());
        let machine = chain
            .machine(&format!("sdk-refused-{index}"), &definition, &role_arn)
            .await;
        let (_execution, described) = chain.ran(&machine, &json!({}), case).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(*expected)),
            "case {case:?}: expected {expected}, got {described:?}"
        );
        assert!(
            described
                .get("cause")
                .and_then(Value::as_str)
                .is_some_and(|cause| !cause.is_empty()),
            "case {case:?}: the failure says what happened, got {described:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The same call inside a DISTRIBUTED `Map` state's child branch
// ---------------------------------------------------------------------------

/// The error name S3's own client raises for a key that is not there, which is what the
/// table above pins for a top-level task and the one below pins for an item's own.
const NO_SUCH_KEY: &str = "S3.NoSuchKeyException";

/// How long a row waits for a Map Run to have counted every one of its items.
const MAP_RUN_PATIENCE: Duration = Duration::from_secs(60);

/// A state machine whose one DISTRIBUTED `Map` state runs an `s3:getObject` task for
/// every item, over the key that item carries, with the tolerated-failure members
/// `tolerance` declares.
fn mapping_over_reads(items: Value, tolerance: Value) -> Value {
    let Value::Object(tolerance) = tolerance else {
        panic!("expected an object of Map members, got {tolerance:?}")
    };
    let mut state = json!({
        "Type": "Map",
        "Items": items,
        "ItemProcessor": {
            "ProcessorConfig": { "Mode": "DISTRIBUTED", "ExecutionType": "STANDARD" },
            "StartAt": "Gate",
            "States": {
                "Gate": {
                    "Type": "Task",
                    "Resource": "arn:aws:states:::aws-sdk:s3:getObject",
                    "Parameters": { "Bucket": BUCKET, "Key.$": "$.key" },
                    "ResultSelector": { "body.$": "$.Body" },
                    "End": true,
                },
            },
        },
        "End": true,
    });
    for (member, value) in tolerance {
        state[member] = value;
    }
    json!({ "StartAt": "Fan", "States": { "Fan": state } })
}

/// What the object one item addresses holds.
fn held(item: usize) -> String {
    json!({ "item": item }).to_string()
}

/// Poll `DescribeMapRun` until the run has stopped running and every one of its items
/// has been counted, which is the condition the counts below are read at: an item is
/// counted from the instant its own iteration finished, so the run is asked again
/// rather than waited out.
async fn counted(chain: &Chain, map_run_arn: &str, case: &str) -> Value {
    let began = Instant::now();
    loop {
        let described = chain.map_run(map_run_arn).await;
        let running = described.get("status").and_then(Value::as_str) == Some("RUNNING");
        let pending = described
            .pointer("/itemCounts/pending")
            .and_then(Value::as_i64);
        if !running && pending == Some(0) {
            return described;
        }
        assert!(
            began.elapsed() < MAP_RUN_PATIENCE,
            "case {case:?}: expected the Map Run to stop running with every item counted, \
             got {described:?}"
        );
        ::tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// One count of a described Map Run.
fn count(described: &Value, member: &str) -> Option<i64> {
    described
        .pointer(&format!("/itemCounts/{member}"))
        .and_then(Value::as_i64)
}

/// One row of the Map table: how many items the state iterates, which of them address
/// an object nobody stored, and the tolerated-failure members the state declares.
struct MapRow {
    case: &'static str,
    count: usize,
    missing: &'static [usize],
    tolerance: Value,
}

/// An `aws-sdk:s3:getObject` task in the child branch of a DISTRIBUTED `Map` state is
/// called for every item, so each item's own call decides its child execution: the
/// items whose object is there SUCCEED with the object's bytes, the ones whose object is
/// not there FAIL with the error S3's own client raises, the Map Run counts that split,
/// and a redrive of the parent — once the missing objects are stored — re-runs exactly
/// the items whose call had failed, the others keeping the executions they ran as.
///
/// The rows differ along how many items there are, which of them are missing their
/// object (the first, the last, several scattered through the run) and how the state
/// states its failure budget (none at all, a percentage, a count), each row's failures
/// being past that budget so the parent is redrivable at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_item_of_a_distributed_map_runs_its_own_sdk_s3_call() {
    let rows = [
        MapRow {
            case: "one item of three has no object, and the state tolerates no failure",
            count: 3,
            missing: &[1],
            tolerance: json!({}),
        },
        MapRow {
            case: "the first and the last of four, past a tolerated percentage",
            count: 4,
            missing: &[0, 3],
            tolerance: json!({ "ToleratedFailurePercentage": 25 }),
        },
        MapRow {
            case: "three of nine scattered through the run, past a tolerated count",
            count: 9,
            missing: &[2, 4, 7],
            tolerance: json!({ "ToleratedFailureCount": 2 }),
        },
    ];

    let chain = Chain::new();
    chain.bucket(BUCKET).await;
    let role_arn = chain.role("SdkMapRole", &every_action()).await;
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // One object per item that is meant to succeed; an item in `missing` addresses
        // a key nothing stored, which is what makes that item's own call fail.
        let keys: Vec<String> = (0..row.count)
            .map(|item| format!("map/{index}/item-{item}"))
            .collect();
        for (item, key) in keys.iter().enumerate() {
            if !row.missing.contains(&item) {
                chain.object(key, &held(item)).await;
            }
        }
        let items: Vec<Value> = keys
            .iter()
            .enumerate()
            .map(|(item, key)| json!({ "id": item, "key": key }))
            .collect();
        let definition = mapping_over_reads(Value::Array(items), row.tolerance.clone());
        let machine = chain
            .machine(&format!("sdk-map-{index}"), &definition, &role_arn)
            .await;
        let (execution, described) = chain.ran(&machine, &json!({}), case).await;
        // More items failed than the state tolerates, so the state's own error is the
        // threshold one rather than any one item's.
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(TOLERATED_FAILURE_ERROR)),
            "case {case:?}: expected the failures to be weighed against the state's threshold, \
             got {described:?}"
        );

        let map_run_arn = chain.map_run_arn(&execution, case).await;
        let first = counted(&chain, &map_run_arn, case).await;
        let failed = row.missing.len() as i64;
        let total = row.count as i64;
        assert_eq!(
            (
                first.get("status").and_then(Value::as_str),
                count(&first, "succeeded"),
                count(&first, "failed"),
                count(&first, "total"),
            ),
            (Some("FAILED"), Some(total - failed), Some(failed), Some(total)),
            "case {case:?}: expected the Map Run to count the split the calls decided, \
             got {first:?}"
        );

        let before = chain.children(&map_run_arn).await;
        assert_eq!(
            before.len(),
            row.count,
            "case {case:?}: every item ran as a child execution, got {before:?}"
        );
        for item in 0..row.count {
            let child = &before[&item.to_string()];
            let missing = row.missing.contains(&item);
            let expected = match missing {
                true => "FAILED",
                false => "SUCCEEDED",
            };
            assert_eq!(
                child.get("status").and_then(Value::as_str),
                Some(expected),
                "case {case:?}: item {item} ended {expected}, got {child:?}"
            );
            match missing {
                // The item's own call was made and S3 itself refused it, so the child
                // carries that service's error — not a report that nothing was run.
                true => {
                    let error = child.get("error").and_then(Value::as_str);
                    assert_eq!(
                        error,
                        Some(NO_SUCH_KEY),
                        "case {case:?}: item {item}'s own call was refused by S3, got {child:?}"
                    );
                    assert_ne!(
                        error,
                        Some(RUNTIME_ERROR),
                        "case {case:?}: item {item}'s integration must be dispatched rather \
                         than reported as work nothing ran, got {child:?}"
                    );
                }
                // ...and an item whose object is there puts out the bytes S3 answered.
                false => {
                    let output: Value = child
                        .get("output")
                        .and_then(Value::as_str)
                        .map(|text| serde_json::from_str(text).expect("a child's output is JSON"))
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: expected item {item} to put out {child:?}")
                        });
                    assert_eq!(
                        output.get("body").and_then(Value::as_str),
                        Some(held(item).as_str()),
                        "case {case:?}: item {item} read the object S3 holds, got {output:?}"
                    );
                }
            }
        }

        // The objects the failed items addressed are stored — what a caller redriving
        // such a run puts right — and the parent is redriven.
        for item in row.missing {
            chain.object(&keys[*item], &held(*item)).await;
        }
        chain
            .sfn("RedriveExecution", &json!({ "executionArn": execution }))
            .await;
        let described = chain.settled(&execution, case).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("redriveCount").and_then(Value::as_i64),
            ),
            (Some("SUCCEEDED"), Some(1)),
            "case {case:?}: expected the redriven run to succeed, got {described:?}"
        );

        let after_run = counted(&chain, &map_run_arn, case).await;
        assert_eq!(
            (
                after_run.get("status").and_then(Value::as_str),
                count(&after_run, "succeeded"),
                count(&after_run, "failed"),
                after_run.get("redriveCount").and_then(Value::as_i64),
            ),
            (Some("SUCCEEDED"), Some(total), Some(0), Some(1)),
            "case {case:?}: expected the same Map Run to count every item succeeded, \
             got {after_run:?}"
        );

        let after = chain.children(&map_run_arn).await;
        assert_eq!(
            after.len(),
            row.count,
            "case {case:?}: the redriven run holds the same items, got {after:?}"
        );
        for item in 0..row.count {
            let id = item.to_string();
            let (was, now) = (&before[&id], &after[&id]);
            assert_eq!(
                now.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: item {item} succeeded on the redriven attempt, got {now:?}"
            );
            let ran_again = was.get("executionArn") != now.get("executionArn");
            assert_eq!(
                ran_again,
                row.missing.contains(&item),
                "case {case:?}: item {item} ran again exactly if its own call had failed: \
                 was {was:?}, now {now:?}"
            );
            if !ran_again {
                assert_eq!(
                    (was.get("startDate"), was.get("output")),
                    (now.get("startDate"), now.get("output")),
                    "case {case:?}: item {item} had succeeded, so its work is not repeated: \
                     was {was:?}, now {now:?}"
                );
            }
        }
    }
}
