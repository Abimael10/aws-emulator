// SPDX-License-Identifier: CC0-1.0

//! What a cluster ADMITS a `spark-submit` step to, end to end over the wire.
//!
//! THE RULE: a Spark application submitted to a cluster runs its executors, and in CLUSTER
//! deploy mode its driver, inside containers the cluster's resource manager allocates, so a
//! submission is admitted only while every container it asks for fits the largest one that
//! cluster can allocate — and `spark-submit` refuses one that does not, before anything of
//! the application runs, so the step carrying it ends in that refusal. In CLIENT deploy mode
//! the driver runs in the submitting process rather than in a container, so only the small
//! application master that mode asks for is held to the ceiling; the executors are asked for
//! in both modes and are held to it either way. A submission the cluster admits ends the step
//! `COMPLETED` ("for a step to be considered complete, the main function must exit with a
//! zero exit code", `AddJobFlowSteps` doc).
//!
//! The rows span what that rule generalises over: the deploy mode (cluster, client, and the
//! default a submission that names none runs in), WHERE the size comes from (the command
//! line's own option, a `--conf` property, the cluster's own `spark-defaults` configuration,
//! and the property's documented default), which container is oversized (the driver, the
//! executors, and the off-heap memory an executor asks for beside its own), the unit the size
//! is spelled in, and the ceiling itself — one submission exactly on it, one a single MiB
//! past it, and one asking for more than a size can state.
//!
//! Every outcome is read back through `ListSteps` and `DescribeStep`, so nothing here is
//! evidence of anything but the persisted state.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{at, Emr, INSTANCE_PROFILE, SERVICE_ROLE};

/// How long a poll waits for every step of the queue to reach a terminal state before it
/// gives up: far above both provisioning phases and the steps' own spans, so a loaded host
/// delays the wait rather than failing it.
const QUEUE_DEADLINE: Duration = Duration::from_secs(90);

/// One look at the queue inside [`QUEUE_DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
const TERMINAL: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// The memory an `m5.xlarge` node of an Amazon EMR cluster offers its containers:
/// `yarn.nodemanager.resource.memory-mb` as Amazon EMR's published task-configuration
/// defaults state it for that instance type, which is also the largest single container such
/// a node can hold.
const M5_XLARGE_CONTAINER_MIB: i64 = 12288;

/// The same figure for an `m5.4xlarge`, the type the PRIMARY node of the multi-node cluster
/// below runs: far above the one above, so a ceiling taken from the primary node instead of
/// from the nodes that run containers would admit submissions these rows require refused.
const M5_4XLARGE_CONTAINER_MIB: i64 = 57344;

/// The oversized `spark.driver.memory` the multi-node cluster below SETS in its own
/// `spark-defaults` configuration, in MiB and as the configuration spells it.
const CONFIGURED_DRIVER_MIB: i64 = 20 * 1024;
const CONFIGURED_DRIVER_MEMORY: &str = "20G";

/// The largest container memory a submission can ask for at all: a pebibyte in MiB, the
/// figure a size above it is read as.
const MAX_SUBMITTED_MIB: i64 = 1024 * 1024 * 1024;

/// How `spark-submit` refuses a submission whose application master does not fit, with the
/// figures of this row's own arithmetic.
fn am_refusal(memory: i64, overhead: i64, maximum: i64) -> String {
    format!(
        "Required AM memory ({memory}+{overhead} MB) is above the max threshold ({maximum} MB) \
         of this cluster! Please increase the value of 'yarn.scheduler.maximum-allocation-mb'."
    )
}

/// How it refuses one whose executors do not fit.
fn executor_refusal(
    memory: i64,
    off_heap: i64,
    overhead: i64,
    pyspark: i64,
    maximum: i64,
) -> String {
    format!(
        "Required executor memory ({memory} MB), offHeap memory ({off_heap}) MB, overhead \
         ({overhead} MB), and PySpark memory ({pyspark} MB) is above the max threshold \
         ({maximum} MB) of this cluster! Please check the values of \
         'yarn.scheduler.maximum-allocation-mb' and/or 'yarn.nodemanager.resource.memory-mb'."
    )
}

/// What a row's step must end as.
enum Ends {
    /// The cluster admitted the submission.
    Completed,
    /// It refused it, with this message in the step's `FailureDetails`.
    Refused(String),
}

/// One row: the `spark-submit` a step submits, and the end it must reach.
struct Case {
    label: &'static str,
    /// The words of the submission after `spark-submit` itself.
    submit: Vec<String>,
    ends: Ends,
}

/// The job every row submits: the example application of the cluster's own Spark
/// installation, which is a jar and a main class rather than anything this suite supplies.
fn job() -> Vec<String> {
    [
        "--class",
        "org.apache.spark.examples.SparkPi",
        "/usr/lib/spark/examples/jars/spark-examples.jar",
        "10",
    ]
    .iter()
    .map(|word| (*word).to_string())
    .collect()
}

/// `words` followed by the job every row submits.
fn submitting(words: &[&str]) -> Vec<String> {
    let mut submit: Vec<String> = words.iter().map(|word| (*word).to_string()).collect();
    submit.extend(job());
    submit
}

/// The rows of the multi-node cluster, whose container ceiling is its CORE node's.
fn cases() -> Vec<Case> {
    let maximum = M5_XLARGE_CONTAINER_MIB;
    vec![
        Case {
            label: "the configured driver, in cluster deploy mode",
            submit: submitting(&["--deploy-mode", "cluster"]),
            // The default overhead is a tenth of the container's own memory
            // (`spark.driver.memoryOverheadFactor`, 0.10).
            ends: Ends::Refused(am_refusal(
                CONFIGURED_DRIVER_MIB,
                CONFIGURED_DRIVER_MIB / 10,
                maximum,
            )),
        },
        Case {
            label: "the same submission right-sized on the command line",
            submit: submitting(&[
                "--deploy-mode",
                "cluster",
                "--driver-memory",
                "2G",
                "--executor-memory",
                "4G",
                "--conf",
                "spark.yarn.executor.memoryOverhead=1G",
                "--num-executors",
                "1",
            ]),
            ends: Ends::Completed,
        },
        Case {
            label: "the same configured driver, in client deploy mode",
            submit: submitting(&["--deploy-mode", "client"]),
            ends: Ends::Completed,
        },
        Case {
            label: "a submission that names no deploy mode at all",
            submit: submitting(&[]),
            ends: Ends::Completed,
        },
        Case {
            label: "an oversized driver named on the command line",
            submit: submitting(&["--deploy-mode", "cluster", "--driver-memory", "64g"]),
            ends: Ends::Refused(am_refusal(64 * 1024, 64 * 1024 / 10, maximum)),
        },
        Case {
            label: "a driver whose container is exactly the ceiling",
            submit: submitting(&[
                "--deploy-mode",
                "cluster",
                "--driver-memory",
                "11264m",
                "--conf",
                "spark.driver.memoryOverhead=1024",
            ]),
            ends: Ends::Completed,
        },
        Case {
            label: "a driver whose container is one MiB past the ceiling",
            submit: submitting(&[
                "--deploy-mode",
                "cluster",
                "--driver-memory",
                "11265m",
                "--conf",
                "spark.driver.memoryOverhead=1024",
            ]),
            ends: Ends::Refused(am_refusal(11265, 1024, maximum)),
        },
        Case {
            label: "oversized executors, which client deploy mode asks for too",
            submit: submitting(&["--deploy-mode", "client", "--executor-memory", "32g"]),
            ends: Ends::Refused(executor_refusal(
                32 * 1024,
                0,
                32 * 1024 / 10,
                0,
                maximum,
            )),
        },
        Case {
            label: "off-heap memory that tips the executor past the ceiling",
            submit: submitting(&[
                "--deploy-mode",
                "client",
                "--executor-memory",
                "8g",
                "--conf",
                "spark.memory.offHeap.enabled=true",
                "--conf",
                "spark.memory.offHeap.size=4g",
            ]),
            ends: Ends::Refused(executor_refusal(
                8 * 1024,
                4 * 1024,
                8 * 1024 / 10,
                0,
                maximum,
            )),
        },
        Case {
            label: "a driver larger than the unit a size is stated in holds",
            submit: submitting(&[
                "--deploy-mode",
                "cluster",
                "--driver-memory",
                "9223372036854775807",
            ]),
            // A size above the largest one the reading answers is taken AS that one — a
            // pebibyte in MiB — which no cluster can allocate.
            ends: Ends::Refused(am_refusal(
                MAX_SUBMITTED_MIB,
                MAX_SUBMITTED_MIB / 10,
                maximum,
            )),
        },
    ]
}

/// The `StepConfig` one row submits.
fn step_config(case: &Case) -> Value {
    let mut args = vec!["spark-submit".to_string()];
    args.extend(case.submit.iter().cloned());
    json!({
        "Name": case.label,
        "ActionOnFailure": "CONTINUE",
        "HadoopJarStep": { "Jar": "command-runner.jar", "Args": args },
    })
}

/// The cluster every row of [`cases`] is submitted to: a primary node far larger than the one
/// CORE node that runs its containers, and a `spark-defaults` configuration of its own that
/// oversizes the driver for every submission that does not override it.
fn multi_node_cluster(name: &str) -> Value {
    json!({
        "Name": name,
        "ReleaseLabel": "emr-6.15.0",
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Applications": [{ "Name": "Spark" }],
        // Every row runs at once, so the queue settles in one span rather than one per step.
        "StepConcurrencyLevel": 16,
        "Configurations": [{
            "Classification": "spark-defaults",
            "Properties": { "spark.driver.memory": CONFIGURED_DRIVER_MEMORY },
        }],
        "Instances": {
            "KeepJobFlowAliveWhenNoSteps": true,
            "InstanceGroups": [
                { "InstanceRole": "MASTER", "InstanceType": "m5.4xlarge", "InstanceCount": 1 },
                { "InstanceRole": "CORE", "InstanceType": "m5.xlarge", "InstanceCount": 1 },
            ],
        },
    })
}

/// Wait until every step of `cluster_id` is terminal, answering them by step id.
///
/// The wait is for the CONDITION, re-read until it holds or [`QUEUE_DEADLINE`] passes.
async fn drained(case: &str, emr: &Emr, cluster_id: &str) -> Vec<Value> {
    let deadline = Instant::now() + QUEUE_DEADLINE;
    loop {
        let listed = emr.list_steps(json!({ "ClusterId": cluster_id })).await;
        let steps: Vec<Value> = listed["Steps"].as_array().cloned().unwrap_or_default();
        let settled = !steps.is_empty()
            && steps.iter().all(|step| {
                at(step, &["Status", "State"])
                    .as_str()
                    .is_some_and(|state| TERMINAL.contains(&state))
            });
        if settled {
            return steps;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every step must reach a terminal state, got {steps:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The step named `name` among `steps`.
fn step_named<'a>(case: &str, steps: &'a [Value], name: &str) -> &'a Value {
    steps
        .iter()
        .find(|step| step["Name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("case {case:?}: no step named {name:?} among {steps:?}"))
}

/// Assert one row's step ended the way the rule requires.
fn assert_ends(case: &Case, step: &Value) {
    let label = case.label;
    let state = at(step, &["Status", "State"]).as_str().unwrap_or_default();
    let details = at(step, &["Status", "FailureDetails"]).clone();
    match &case.ends {
        Ends::Completed => {
            assert_eq!(
                state, "COMPLETED",
                "case {label:?}: an admitted submission completes, got {state:?} with {details}"
            );
            assert!(
                details.is_null(),
                "case {label:?}: a completed step reports no failure, got {details}"
            );
        }
        Ends::Refused(message) => {
            assert_eq!(
                state, "FAILED",
                "case {label:?}: a refused submission fails, got {state:?}"
            );
            let answered = at(&details, &["Message"]).as_str().unwrap_or_default();
            assert_eq!(
                answered, message.as_str(),
                "case {label:?}: expected {message:?}, got {answered:?}"
            );
            assert!(
                !at(&details, &["Reason"]).as_str().unwrap_or_default().is_empty(),
                "case {label:?}: a failed step states a reason, got {details}"
            );
        }
    }
    // Either way the step really RAN the lifecycle: both instants of its timeline are
    // reported.
    for member in ["StartDateTime", "EndDateTime"] {
        assert!(
            !at(step, &["Status", "Timeline", member]).is_null(),
            "case {label:?}: a step that ended reports its {member}, got {step}"
        );
    }
}

/// THE RULE, over the table: which submissions the cluster's own capacity admits, and what
/// each step therefore ends in.
#[tokio::test]
async fn a_cluster_admits_only_the_submissions_its_containers_fit() {
    let emr = Emr::new();
    let cases = cases();
    let cluster_id = emr
        .run_job_flow(multi_node_cluster("submission admission"))
        .await;
    let submitted = emr
        .ok(
            "AddJobFlowSteps",
            json!({
                "JobFlowId": &cluster_id,
                "Steps": cases.iter().map(step_config).collect::<Vec<Value>>(),
            }),
        )
        .await;
    assert_eq!(
        submitted["StepIds"].as_array().map(Vec::len),
        Some(cases.len()),
        "AddJobFlowSteps must accept every row, got {submitted}"
    );
    let steps = drained("submission admission", &emr, &cluster_id).await;
    for case in &cases {
        assert_ends(case, step_named("submission admission", &steps, case.label));
    }
}

/// THE SAME RULE where the cluster has no node but its primary: such a cluster runs its
/// containers there, so its own primary node is the ceiling — the same driver that the
/// multi-node cluster above refuses on its smaller CORE node is admitted here, and one past
/// THIS node's own ceiling is refused.
#[tokio::test]
async fn a_single_node_cluster_is_held_to_its_primary_node() {
    let emr = Emr::new();
    let cases = vec![
        Case {
            label: "a driver this larger node fits",
            submit: submitting(&["--deploy-mode", "cluster", "--driver-memory", "20g"]),
            ends: Ends::Completed,
        },
        Case {
            label: "a driver no node of this cluster fits",
            submit: submitting(&["--deploy-mode", "cluster", "--driver-memory", "60g"]),
            ends: Ends::Refused(am_refusal(
                60 * 1024,
                60 * 1024 / 10,
                M5_4XLARGE_CONTAINER_MIB,
            )),
        },
    ];
    let cluster_id = emr
        .run_job_flow(json!({
            "Name": "a single node cluster",
            "ReleaseLabel": "emr-6.15.0",
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "Applications": [{ "Name": "Spark" }],
            "StepConcurrencyLevel": 4,
            "Instances": {
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceGroups": [
                    { "InstanceRole": "MASTER", "InstanceType": "m5.4xlarge", "InstanceCount": 1 },
                ],
            },
        }))
        .await;
    emr.ok(
        "AddJobFlowSteps",
        json!({
            "JobFlowId": &cluster_id,
            "Steps": cases.iter().map(step_config).collect::<Vec<Value>>(),
        }),
    )
    .await;
    let steps = drained("a single node cluster", &emr, &cluster_id).await;
    for case in &cases {
        assert_ends(case, step_named("a single node cluster", &steps, case.label));
    }
}

/// A refused submission is read back the same way through `DescribeStep` as through
/// `ListSteps`: the admission is answered once and stored, so two reads of a settled step
/// report the same thing.
#[tokio::test]
async fn a_settled_submission_reads_back_the_same_through_both_readers() {
    let emr = Emr::new();
    let cluster_id = emr
        .run_job_flow(multi_node_cluster("one submission, two readers"))
        .await;
    let case = Case {
        label: "the configured driver, in cluster deploy mode",
        submit: submitting(&["--deploy-mode", "cluster"]),
        ends: Ends::Refused(am_refusal(
            CONFIGURED_DRIVER_MIB,
            CONFIGURED_DRIVER_MIB / 10,
            M5_XLARGE_CONTAINER_MIB,
        )),
    };
    let submitted = emr
        .ok(
            "AddJobFlowSteps",
            json!({ "JobFlowId": &cluster_id, "Steps": [step_config(&case)] }),
        )
        .await;
    let step_id = submitted["StepIds"][0]
        .as_str()
        .expect("AddJobFlowSteps answers a step id")
        .to_string();
    let listed = drained("one submission, two readers", &emr, &cluster_id).await;
    assert_ends(&case, &listed[0]);
    let described = emr
        .ok(
            "DescribeStep",
            json!({ "ClusterId": &cluster_id, "StepId": &step_id }),
        )
        .await["Step"]
        .clone();
    assert_ends(&case, &described);
    // And again, so a read of a settled step changes nothing about it.
    let again = emr
        .ok(
            "DescribeStep",
            json!({ "ClusterId": &cluster_id, "StepId": &step_id }),
        )
        .await["Step"]
        .clone();
    assert_eq!(
        at(&again, &["Status"]),
        at(&described, &["Status"]),
        "a second read of a settled step reports the same status, got {again}"
    );
}
