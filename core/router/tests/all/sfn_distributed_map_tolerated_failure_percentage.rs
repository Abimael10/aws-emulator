// SPDX-License-Identifier: CC0-1.0

//! What a Distributed `Map` state's TOLERATED FAILURE PERCENTAGE decides, driven across the
//! two crates the question spans (Step Functions runs the map, S3 holds the dataset the
//! state's `ItemReader` reads and the markers its items write), mounted on ONE engine exactly
//! as `core/router/src/lib.rs` mounts them.
//!
//! RULE: a Distributed `Map` state's tolerated-failure PERCENTAGE is a budget on the SHARE of
//! its items that failed — the execution settles on a terminal status either way, `FAILED`
//! with `States.ExceedToleratedFailureThreshold` when that share is past the percentage and
//! `SUCCEEDED` when it is within it — and the items that completed keep the work they did, so
//! a breached budget rolls nothing back. The Amazon States Language is not in the Smithy
//! model, so the error name and the state member are the recorded ones; `DescribeMapRun`
//! models what the run carries them as (`toleratedFailurePercentage`, range 0..100, default 0,
//! beside the `itemCounts` the run reports).
//!
//! The rows differ along what the rule generalises over: how many items the dataset holds, how
//! many of them fail and which, the percentage the state runs under (none at all, a share
//! under the failed one, exactly the failed one, a fractional share either side of it, the
//! whole of them), and whether the definition writes that percentage as a literal member or
//! reads it out of the execution input through the member's `*Path` form.
//!
//! The work is counted where it LANDS rather than inferred from the run's own records: an item
//! that completes writes one S3 object named after it and one that fails writes none, so the
//! objects the prefix holds afterwards are exactly the items whose work stands.
//!
//! Which items those are is made DETERMINISTIC by the dataset rather than assumed: the state
//! declares no `MaxConcurrency` — "the default value is 0, which places no quota" — so every
//! item is started together, and an item that is to fail WAITS before it fails while an item
//! that is to complete writes straight away. So an item that was to complete had completed
//! before the failing one reported anything, whether the budget was breached around it or not,
//! and a run that rolled a completed item's work back (or never did it) is a run with a marker
//! missing.
//!
//! Sibling coverage, which this suite does not re-table: `map_run_failure_budget` in the Step
//! Functions crate pins what a Map Run COUNTS as its items finish and that it keeps RUNNING
//! over a failure inside its budget; this suite pins the terminal status the execution settles
//! on and the work that is still in S3 afterwards.
//!
//! The harness (`Chain`, which mounts the two services over one engine, `BUCKET`, `items` and
//! `string`) is the one `sfn_distributed_map_redrive_repeats_only_the_failed_items` builds;
//! `tests/all/main.rs` is this binary's crate root, so it is reached through `crate::<suite>`.
//! The item ids, the dataset keys, the marker prefix and the item's own error name are this
//! suite's own request values; what is asserted about them is only what the rule says.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::sfn_distributed_map_redrive_repeats_only_the_failed_items::{
    items, string, Chain, BUCKET,
};

/// The error a Map Run past the failures it tolerates reports, written out as the recorded
/// wording rather than read from the service.
const TOLERATED_FAILURE_ERROR: &str = "States.ExceedToleratedFailureThreshold";

/// The statuses a run and its children settle on.
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";

/// How far the percentage the run reports may sit from the one the definition asked for: the
/// budget is carried as a single-precision figure, so a fractional share round-trips to within
/// this.
const PERCENTAGE_TOLERANCE: f64 = 1e-4;

/// How long an item that is to FAIL waits before failing, while an item that is to complete
/// writes its work straight away: what orders the two kinds of item within one run, so the
/// work that stands afterwards is decided by the dataset and not by how fast the host ran.
const LINGER_SECONDS: u64 = 2;

/// The inner workflow every item runs: an item that is to succeed WRITES one object named
/// after itself — the work whose survival is counted — and one that is not fails before
/// writing anything, so the markers left behind are exactly the items that completed.
fn marking_processor() -> Value {
    json!({
        "ProcessorConfig": { "Mode": "DISTRIBUTED", "ExecutionType": "STANDARD" },
        "StartAt": "Decide",
        "States": {
            "Decide": {
                "Type": "Choice",
                "Choices": [{ "Variable": "$.ok", "BooleanEquals": true, "Next": "Mark" }],
                "Default": "Linger",
            },
            "Mark": {
                "Type": "Task",
                "Resource": "arn:aws:states:::aws-sdk:s3:putObject",
                "Parameters": {
                    "Bucket": BUCKET,
                    "Key.$": "States.Format('tolerated/{}/{}', $.run, $.id)",
                    "Body": "ran",
                },
                "ResultPath": "$.wrote",
                "End": true,
            },
            // The item that is to fail takes longer than the ones that complete, so every
            // item that was to complete had done so by the time this one reported.
            "Linger": { "Type": "Wait", "Seconds": LINGER_SECONDS, "Next": "Boom" },
            "Boom": { "Type": "Fail", "Error": "ItemRejected", "Cause": "the item said so" },
        },
    })
}

/// A state machine whose one Distributed `Map` state reads its items from the S3 object
/// `dataset_key` holds and runs under the tolerated-failure members `budget` carries.
fn mapping(dataset_key: &str, budget: Value) -> Value {
    let Value::Object(budget) = budget else {
        panic!("expected an object of Map members, got {budget:?}")
    };
    let mut state = json!({
        "Type": "Map",
        "ItemReader": {
            "Resource": "arn:aws:states:::s3:getObject",
            "ReaderConfig": { "InputType": "JSON" },
            "Parameters": { "Bucket": BUCKET, "Key": dataset_key },
        },
        "ItemProcessor": marking_processor(),
        "End": true,
    });
    let members = state
        .as_object_mut()
        .expect("the Map state is a JSON object");
    for (member, value) in budget {
        members.insert(member, value);
    }
    json!({ "StartAt": "Fan", "States": { "Fan": state } })
}

/// Where the percentage the state runs under is written: the member itself, or the `*Path`
/// form of it, which reads the figure out of the execution's input.
#[derive(Debug, Clone, Copy)]
enum Budget {
    Declared,
    Referenced,
}

/// The member the `Map` state carries, and the execution input the start sends, for a state
/// whose budget is `percentage` written `through`.
fn budget_and_input(through: Budget, percentage: f64) -> (Value, Value) {
    match through {
        Budget::Declared => (
            json!({ "ToleratedFailurePercentage": percentage }),
            json!({}),
        ),
        Budget::Referenced => (
            json!({ "ToleratedFailurePercentagePath": "$.budget" }),
            json!({ "budget": percentage }),
        ),
    }
}

/// The item ids the markers under `prefix` name: the last segment of each key, which the
/// inner workflow wrote the item's own id into.
fn marked_items(keys: &[String], case: &str) -> BTreeSet<usize> {
    keys.iter()
        .map(|key| {
            key.rsplit('/')
                .next()
                .and_then(|id| id.parse::<usize>().ok())
                .unwrap_or_else(|| panic!("case {case:?}: expected an item id in the key {key:?}"))
        })
        .collect()
}

/// One row: a dataset, which of its items fail, the budget the state runs under and how the
/// execution must settle.
struct Row {
    case: &'static str,
    count: usize,
    failing: &'static [usize],
    percentage: f64,
    through: Budget,
    expected: &'static str,
}

/// A Distributed `Map` state settles its execution on the budget its tolerated-failure
/// percentage states, and the items that completed keep their work either way.
///
/// The rows span the dataset's size, how many of its items fail and which, the percentage the
/// state runs under — including a share that is exactly the failed one, a fractional share
/// either side of it, and each end of the member's own 0..100 range — and whether that
/// percentage is written in the state or read out of the execution input. The recorded case
/// this suite's issue was filed about, a dataset of which one item in ten fails run first with
/// no budget at all and then inside a ten-percent one, is two of the rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tolerated_failure_percentage_decides_whether_a_distributed_map_run_fails() {
    let rows: &[Row] = &[
        Row {
            case: "one item in ten fails with no budget at all",
            count: 10,
            failing: &[3],
            percentage: 0.0,
            through: Budget::Declared,
            expected: FAILED,
        },
        Row {
            case: "one item in ten fails inside a ten-percent budget",
            count: 10,
            failing: &[3],
            percentage: 10.0,
            through: Budget::Declared,
            expected: SUCCEEDED,
        },
        Row {
            case: "one item in ten fails past a five-percent budget",
            count: 10,
            failing: &[7],
            percentage: 5.0,
            through: Budget::Declared,
            expected: FAILED,
        },
        Row {
            case: "two items in four fail inside a half budget",
            count: 4,
            failing: &[0, 3],
            percentage: 50.0,
            through: Budget::Declared,
            expected: SUCCEEDED,
        },
        Row {
            case: "two items in four fail past a quarter budget",
            count: 4,
            failing: &[1, 2],
            percentage: 25.0,
            through: Budget::Declared,
            expected: FAILED,
        },
        Row {
            case: "one item in three fails past a 33.3 percent budget",
            count: 3,
            failing: &[1],
            percentage: 33.3,
            through: Budget::Declared,
            expected: FAILED,
        },
        Row {
            case: "one item in three fails inside a 33.4 percent budget",
            count: 3,
            failing: &[1],
            percentage: 33.4,
            through: Budget::Declared,
            expected: SUCCEEDED,
        },
        Row {
            case: "the only item fails inside the whole budget",
            count: 1,
            failing: &[0],
            percentage: 100.0,
            through: Budget::Declared,
            expected: SUCCEEDED,
        },
        Row {
            case: "the only item fails past a ninety-nine percent budget",
            count: 1,
            failing: &[0],
            percentage: 99.0,
            through: Budget::Declared,
            expected: FAILED,
        },
        Row {
            case: "no item fails with no budget at all",
            count: 3,
            failing: &[],
            percentage: 0.0,
            through: Budget::Declared,
            expected: SUCCEEDED,
        },
        Row {
            case: "one item in ten fails inside a budget read from the input",
            count: 10,
            failing: &[5],
            percentage: 10.0,
            through: Budget::Referenced,
            expected: SUCCEEDED,
        },
        Row {
            case: "one item in ten fails past a budget read from the input",
            count: 10,
            failing: &[5],
            percentage: 9.0,
            through: Budget::Referenced,
            expected: FAILED,
        },
    ];

    let chain = Chain::new();
    chain.bucket().await;
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let run = format!("tolerated-{index}");
        let dataset_key = format!("datasets/{run}.json");
        chain
            .object(&dataset_key, &items(row.count, row.failing, &run).to_string())
            .await;
        let (budget, input) = budget_and_input(row.through, row.percentage);
        let machine = chain
            .machine(&run, &mapping(&dataset_key, budget))
            .await;
        let started = chain
            .sfn(
                "StartExecution",
                &json!({
                    "stateMachineArn": machine,
                    "name": "only",
                    "input": input.to_string(),
                }),
            )
            .await;
        let execution = string(&started, "executionArn");

        // The run SETTLES: whichever side of its budget the failures fell, the execution
        // leaves RUNNING rather than hanging on the items it started.
        let described = chain.settled(&execution, case).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some(row.expected),
            "case {case:?}: expected {}, got {described:?}",
            row.expected
        );
        let error = described.get("error").and_then(Value::as_str);
        match row.expected == FAILED {
            // A breached budget is the Map state's own refusal, under the one error name the
            // service reports for it — not the error of the item that happened to fail.
            true => assert_eq!(
                error,
                Some(TOLERATED_FAILURE_ERROR),
                "case {case:?}: expected the failure to be named {TOLERATED_FAILURE_ERROR}, got \
                 {described:?}"
            ),
            false => assert_eq!(
                error, None,
                "case {case:?}: a run inside its budget reports no error, got {described:?}"
            ),
        }

        // The work that landed: a marker for an item that completed, none for one that
        // failed. Every item that was to complete had completed before the failing one
        // reported, so a breached budget must have rolled none of that work back.
        let keys = chain.keys_under(&format!("tolerated/{run}/")).await;
        let kept = marked_items(&keys, case);
        let completable: BTreeSet<usize> = (0..row.count)
            .filter(|id| !row.failing.contains(id))
            .collect();
        assert_eq!(
            kept, completable,
            "case {case:?}: the items that completed keep their work, breached budget or not, \
             and an item that failed wrote none, got {kept:?} against {completable:?}"
        );

        // And the run's own records agree with that work: the children that succeeded are
        // exactly the items whose work stands, a child of a failing item FAILED, and the Map
        // Run counts them under the budget it weighed them against.
        let listed = chain.map_runs(&execution).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: one Distributed Map state is one Map Run, got {listed:?}"
        );
        let map_run_arn = string(&listed[0], "mapRunArn");
        let children = chain.children(&map_run_arn).await;
        let mut succeeded_children = BTreeSet::new();
        for (id, attempts) in &children {
            let child = attempts
                .last()
                .unwrap_or_else(|| panic!("case {case:?}: expected a child for item {id}"));
            let status = string(child, "status");
            let item = usize::try_from(*id).expect("an item id fits");
            let expected_child = match row.failing.contains(&item) {
                true => FAILED,
                false => SUCCEEDED,
            };
            assert_eq!(
                status, expected_child,
                "case {case:?}: item {item}'s child ends as its own item did, got {child:?}"
            );
            if status == SUCCEEDED {
                succeeded_children.insert(item);
            }
        }
        assert_eq!(
            succeeded_children, kept,
            "case {case:?}: the children that succeeded are the items whose work stands, got \
             {succeeded_children:?} against {kept:?}"
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
        let kept_count = i64::try_from(kept.len()).unwrap_or(i64::MAX);
        let failed = i64::try_from(row.failing.len()).unwrap_or(i64::MAX);
        assert_eq!(
            (
                described_run.get("status").and_then(Value::as_str),
                counts("total"),
                counts("succeeded"),
                counts("failed"),
            ),
            (
                Some(row.expected),
                Some(total),
                Some(kept_count),
                Some(failed)
            ),
            "case {case:?}: the Map Run settles as its items turned out and counts the work \
             they did, got {described_run:?}"
        );
        let reported = described_run
            .get("toleratedFailurePercentage")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a toleratedFailurePercentage in {described_run:?}")
            });
        assert!(
            (reported - row.percentage).abs() < PERCENTAGE_TOLERANCE,
            "case {case:?}: expected the Map Run to report the budget {} it ran under, got \
             {reported}",
            row.percentage
        );
    }
}
