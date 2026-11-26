// SPDX-License-Identifier: CC0-1.0

//! A cluster runs the queue it accepted ITSELF, with no further call from a client.
//!
//! THE RULE: "Amazon EMR executes each step in the order listed" (`AddJobFlowSteps` doc), from
//! the instant the cluster's capacity is ready, and `DescribeStep` "provides more detail about
//! the cluster step" — it REPORTS what the cluster did rather than being what makes it happen.
//! So every step a launch queued runs, and comes to rest in a terminal `StepState`, whether or
//! not anybody is watching: a client that polls on an interval of its own choosing finds each
//! step settled once the work answered, however coarsely it looks, and the objects the work
//! wrote are there to be read.
//!
//! The suite drives that by making NO Amazon EMR call at all between the launch and the final
//! read: it waits for the LAST step's own output to appear in Amazon S3 — a service that holds
//! none of Amazon EMR's step state — and only then reports the steps. A queue that advances
//! one piece of work per Amazon EMR call could not get there: the second step's output exists
//! only if the first step's answer was recorded and the step behind it started with nobody
//! asking.
//!
//! The rows differ along what the rule generalises over: how many steps are queued behind one
//! another at `StepConcurrencyLevel` 1 (one, two, three), whether a step READS what the step
//! before it wrote, and how a step ENDS (an application that exits cleanly, one that raises
//! under `ActionOnFailure=CONTINUE`, with the queue still draining behind it).
//!
//! Amazon S3 is mounted BESIDE Amazon EMR on one engine, exactly as the router mounts them, so
//! every program is read from the bucket that holds it and every output is a real object.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::spark_step_application::{emr_ok, emulator, put_object, s3, BUCKET};

/// The states a step never leaves (`com.amazonaws.emr#StepState`: `PENDING`,
/// `CANCEL_PENDING`, `RUNNING`, `COMPLETED`, `CANCELLED`, `FAILED`, `INTERRUPTED` — the four
/// a step cannot move out of). Written out here rather than imported, so a wrong set in
/// `src/` cannot make this suite pass.
const TERMINAL: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// How long a row waits for the output its last step writes: both provisioning phases, every
/// step's own span and its application run, with generous slack for a loaded machine (the
/// whole suite settles in well under a tenth of this).
const OUTPUT_DEADLINE: Duration = Duration::from_secs(180);

/// How often it looks again — at Amazon S3, which holds no step state of this service's.
const OUTPUT_POLL: Duration = Duration::from_millis(200);

/// The program every row submits: it writes the text its second argument names under the
/// prefix its third names, having first read the prefix its fourth names when it is given one
/// — so a step can read what the step before it wrote — and RAISES instead when its first
/// argument says to.
const PROGRAM: &str = r#"
import sys

from pyspark.sql import SparkSession

mode, text, output = sys.argv[1], sys.argv[2], sys.argv[3]
source = sys.argv[4] if len(sys.argv) > 4 else ""
spark = SparkSession.builder.appName("queued-step").getOrCreate()
if source:
    text = "%s+%s" % (",".join(sorted(spark.sparkContext.textFile(source).collect())), text)
if mode == "raise":
    raise RuntimeError("the step gave up on %s" % text)
spark.sparkContext.parallelize([text], 1).saveAsTextFile(output)
spark.stop()
"#;

/// One step of a row: what it submits, and the state it must come to rest in.
struct Queued {
    /// `raise` for the step whose application raises, `write` for one that exits cleanly.
    mode: &'static str,
    /// Whether it reads the output of the step before it.
    reads_the_one_before: bool,
    /// The terminal `StepState` it must reach.
    ends: &'static str,
}

/// The key one row's step writes its output under.
fn output_prefix(row: usize, step: usize) -> String {
    format!("queued/{row}/step-{step}")
}

/// The object a completed step's `saveAsTextFile` leaves: one part file per partition, and
/// the program writes one partition.
fn output_object(row: usize, step: usize) -> String {
    format!("{}/part-00000", output_prefix(row, step))
}

/// The `StepConfig` one queued step submits.
fn step_config(row: usize, step: usize, queued: &Queued) -> Value {
    let mut args: Vec<String> = vec![
        "spark-submit".to_string(),
        "--deploy-mode".to_string(),
        "client".to_string(),
        format!("s3://{BUCKET}/queued/program.py"),
        queued.mode.to_string(),
        format!("row-{row}-step-{step}"),
        format!("s3://{BUCKET}/{}", output_prefix(row, step)),
    ];
    if queued.reads_the_one_before {
        // The PART FILE the step before it wrote, named directly: what is asserted is the
        // round trip of one step's output into the next step's input, not how a reader
        // treats a directory's marker files.
        args.push(format!(
            "s3://{BUCKET}/{}",
            output_object(row, step.saturating_sub(1))
        ));
    }
    json!({
        "Name": format!("row {row} step {step}"),
        "ActionOnFailure": "CONTINUE",
        "HadoopJarStep": { "Jar": "command-runner.jar", "Args": args },
    })
}

/// The object at `key`, or `None` when the store holds none.
async fn object(router: &smithy_server_runtime::MultiServiceRouter, key: &str) -> Option<String> {
    let (status, answered) = s3(router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    (200..300)
        .contains(&status)
        .then(|| String::from_utf8_lossy(&answered).to_string())
}

/// Every row: the queue drains with no Amazon EMR call behind the launch, each step ends in
/// what its own work answered, and what a step wrote is what the step after it read.
#[tokio::test]
async fn a_launched_queue_drains_and_settles_without_a_further_call() {
    // (case, the steps queued behind one another)
    let rows: Vec<(&str, Vec<Queued>)> = vec![
        (
            "one step that writes its own output",
            vec![Queued {
                mode: "write",
                reads_the_one_before: false,
                ends: "COMPLETED",
            }],
        ),
        (
            "two steps where the second reads what the first wrote",
            vec![
                Queued {
                    mode: "write",
                    reads_the_one_before: false,
                    ends: "COMPLETED",
                },
                Queued {
                    mode: "write",
                    reads_the_one_before: true,
                    ends: "COMPLETED",
                },
            ],
        ),
        (
            "three steps, the first of which raises",
            vec![
                Queued {
                    mode: "raise",
                    reads_the_one_before: false,
                    ends: "FAILED",
                },
                Queued {
                    mode: "write",
                    reads_the_one_before: false,
                    ends: "COMPLETED",
                },
                Queued {
                    mode: "write",
                    reads_the_one_before: true,
                    ends: "COMPLETED",
                },
            ],
        ),
    ];

    let router = emulator();
    let (status, answered) = s3(&router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
    put_object(&router, "program", "queued/program.py", PROGRAM.as_bytes()).await;

    // Every row's cluster is LAUNCHED first, and no Amazon EMR call is made for any of them
    // again until the work has answered: what drives the queue is the service itself.
    let mut launched: Vec<String> = Vec::new();
    for (ordinal, (case, steps)) in rows.iter().enumerate() {
        let configs: Vec<Value> = steps
            .iter()
            .enumerate()
            .map(|(step, queued)| step_config(ordinal, step, queued))
            .collect();
        let answer = emr_ok(
            &router,
            "RunJobFlow",
            json!({
                "Name": format!("queued-{ordinal}"),
                "ReleaseLabel": "emr-6.15.0",
                "JobFlowRole": "EMR_EC2_DefaultRole",
                "ServiceRole": "EMR_DefaultRole",
                "Applications": [{ "Name": "Hadoop" }, { "Name": "Spark" }],
                "StepConcurrencyLevel": 1,
                "Steps": configs,
                "Instances": {
                    "KeepJobFlowAliveWhenNoSteps": true,
                    "InstanceGroups": [
                        {
                            "Name": "primary",
                            "InstanceRole": "MASTER",
                            "InstanceType": "m5.xlarge",
                            "InstanceCount": 1,
                        },
                        {
                            "Name": "core",
                            "InstanceRole": "CORE",
                            "InstanceType": "m5.xlarge",
                            "InstanceCount": 1,
                        },
                    ],
                },
            }),
        )
        .await;
        launched.push(
            answer["JobFlowId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: RunJobFlow answers a JobFlowId"))
                .to_string(),
        );
    }

    // The LAST object each row's queue has to write, polled for in Amazon S3 — the condition,
    // not a wait of any length. Amazon S3 holds none of this service's step state, so nothing
    // this loop does can advance a queue.
    let deadline = Instant::now() + OUTPUT_DEADLINE;
    for (ordinal, (case, steps)) in rows.iter().enumerate() {
        let last = steps.len().saturating_sub(1);
        let key = output_object(ordinal, last);
        loop {
            if object(&router, &key).await.is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the queue must drain with no further Amazon EMR call, but \
                 {key} was never written"
            );
            tokio::time::sleep(OUTPUT_POLL).await;
        }
    }

    // Only now is Amazon EMR asked anything: every step reports the state its own work
    // answered, and the output of a step that read the one before it carries what that step
    // wrote. The LAST thing the clock owes a step whose work has answered is the end of its
    // own run span ([`crate::state::STEP_RUN_SPAN`]), so the listing is polled for the
    // CONDITION that every step has come to rest rather than read once.
    for (ordinal, (case, steps)) in rows.iter().enumerate() {
        let cluster_id = &launched[ordinal];
        let deadline = Instant::now() + OUTPUT_DEADLINE;
        let reported: Vec<Value> = loop {
            let listed = emr_ok(&router, "ListSteps", json!({ "ClusterId": cluster_id })).await;
            let mut held: Vec<Value> = listed["Steps"].as_array().cloned().unwrap_or_default();
            // `ListSteps` reports the steps newest first; the rows are indexed in submission
            // order.
            held.reverse();
            assert_eq!(
                held.len(),
                steps.len(),
                "case {case:?}: every submitted step must be reported, got {listed}"
            );
            if held.iter().all(|step| {
                TERMINAL.contains(&step["Status"]["State"].as_str().unwrap_or_default())
            }) {
                break held;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: every step must come to rest, got {listed}"
            );
            tokio::time::sleep(OUTPUT_POLL).await;
        };
        for (step, queued) in steps.iter().enumerate() {
            let held = &reported[step];
            let state = held["Status"]["State"].as_str().unwrap_or_default();
            assert_eq!(
                state, queued.ends,
                "case {case:?}: step {step} must end in what its own work answered, got {held}"
            );
            // `DescribeStep` answers for the same record the listing projected.
            let described = emr_ok(
                &router,
                "DescribeStep",
                json!({ "ClusterId": cluster_id, "StepId": held["Id"] }),
            )
            .await;
            assert_eq!(
                described["Step"]["Status"]["State"].as_str(),
                Some(state),
                "case {case:?}: DescribeStep must report the same state, got {described}"
            );
            if queued.ends == "FAILED" {
                let reason = described["Step"]["Status"]["FailureDetails"]["Reason"]
                    .as_str()
                    .unwrap_or_default();
                assert!(
                    !reason.is_empty(),
                    "case {case:?}: a step that failed must say why, got {described}"
                );
            }
            if queued.ends != "COMPLETED" {
                continue;
            }
            let wrote = object(&router, &output_object(ordinal, step)).await;
            let expected = if queued.reads_the_one_before {
                format!("row-{ordinal}-step-{}+row-{ordinal}-step-{step}\n", step - 1)
            } else {
                format!("row-{ordinal}-step-{step}\n")
            };
            assert_eq!(
                wrote.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: step {step} must have written {expected:?}, got {wrote:?}"
            );
        }
    }
}
