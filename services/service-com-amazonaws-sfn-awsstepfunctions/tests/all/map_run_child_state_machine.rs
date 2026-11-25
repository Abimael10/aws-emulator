// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CHILD state machine a Distributed Map state runs, driven
//! over this service's own wire protocol.
//!
//! The rule under test: a Distributed Map state runs its items on a state machine of
//! its own, identified by the Map state's LABEL — the `Label` the state declares, else
//! an identifier the service generates for it — and named by the parent state machine's
//! ARN with that label after a `/`. So
//!
//! 1. every member that reports the state machine a Map Run EXECUTED carries that
//!    labeled ARN, with the same label the Map Run's own `mapRunArn` carries:
//!    `ListMapRuns`' `stateMachineArn`, and the `stateMachineArn` of each child
//!    workflow execution the Map Run started (`ListExecutions`, `DescribeExecution`);
//! 2. `DescribeStateMachine` on that labeled ARN reports the child state machine: the
//!    `ItemProcessor`'s OWN definition (the `ProcessorConfig` that says how the parent
//!    runs it is not part of it), the `type` its `ExecutionType` runs child workflow
//!    executions as, and the `label` the ARN carried;
//! 3. a labeled ARN that names no Distributed Map state of the definition resolves to
//!    nothing, which is this operation's own `StateMachineDoesNotExist`.
//!
//! The table below spans what the rule generalises over: whether the state declares a
//! `Label`, which `ExecutionType` its processor names (or none at all), how the
//! processor's own definition is written (member order and whitespace), and where the
//! state sits in the definition — at the top level, inside a `Parallel` branch, and
//! nested inside another Distributed Map.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{
    create_machine, error_code, is_generated_label, map_run_label, start_execution, string,
    Emulator, ACCOUNT, REGION, ROLE_ARN,
};

/// How long a case waits for a run to leave `RUNNING`.
///
/// Generous: what every case asserts is what the run RECORDED, never how fast the host
/// got there, so each wait below looks for that condition and is bounded only so a run
/// that never settles fails instead of hanging.
const PATIENCE: Duration = Duration::from_secs(120);

/// How often such a wait looks again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// Two items for every definition below, so a Map Run starts more than one child.
const TWO_ITEMS: &str = r#"{"items":[1,2]}"#;

/// `type` and `definition` as `includedData` names them
/// (`crate::state::INCLUDED_DATA_METADATA_ONLY` /
/// `crate::state::METADATA_ONLY_DEFINITION`, the service's own constants).
const METADATA_ONLY: &str = "METADATA_ONLY";
const METADATA_ONLY_DEFINITION: &str = "{}";

/// One child state machine a row's definition must report, in the order the Map states
/// that run them were entered.
struct ChildMachine {
    /// The `Label` the Map state declares, or `None` for a state that declares none —
    /// whose label is the service's own, so no row names its value.
    declared: Option<&'static str>,
    /// The definition `DescribeStateMachine` reports for the labeled ARN.
    definition: &'static str,
    /// The `type` it reports for it.
    machine_type: &'static str,
}

/// One row: a parent definition and the child state machines its Distributed Map states
/// run.
struct Row {
    case: &'static str,
    definition: String,
    children: &'static [ChildMachine],
}

/// The `ItemProcessor` text every row below runs, with the `Mode` fixed to DISTRIBUTED
/// and whatever `ExecutionType` the row names (`None` for a processor that names none).
fn processor(execution_type: Option<&str>) -> String {
    let config = match execution_type {
        Some(execution_type) => {
            format!(r#"{{"Mode":"DISTRIBUTED","ExecutionType":"{execution_type}"}}"#)
        }
        None => r#"{"Mode":"DISTRIBUTED"}"#.to_string(),
    };
    format!(
        r#"{{"ProcessorConfig":{config},"StartAt":"P","States":{{"P":{{"Type":"Pass","End":true}}}}}}"#
    )
}

/// The definition the labeled ARN of such a processor reports: the processor without
/// its `ProcessorConfig`.
const PASS_CHILD: &str = r#"{"StartAt":"P","States":{"P":{"Type":"Pass","End":true}}}"#;

/// A top-level Distributed Map state over two literal items, with the `Label` it
/// declares (if any) and the `ExecutionType` its processor names (if any).
fn top_level(label: Option<&str>, execution_type: Option<&str>) -> String {
    let label = match label {
        Some(label) => format!(r#","Label":"{label}""#),
        None => String::new(),
    };
    format!(
        r#"{{"StartAt":"M","States":{{"M":{{"Type":"Map","Items":[1,2],"ItemProcessor":{},"End":true{label}}}}}}}"#,
        processor(execution_type)
    )
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a state that declares a Label, whose processor runs EXPRESS children",
            definition: top_level(Some("fanout"), Some("EXPRESS")),
            children: &[ChildMachine {
                declared: Some("fanout"),
                definition: PASS_CHILD,
                machine_type: "EXPRESS",
            }],
        },
        Row {
            case: "a state that declares no Label, whose processor runs STANDARD children",
            definition: top_level(None, Some("STANDARD")),
            children: &[ChildMachine {
                declared: None,
                definition: PASS_CHILD,
                machine_type: "STANDARD",
            }],
        },
        Row {
            case: "a processor that names no ExecutionType at all",
            definition: top_level(Some("plain"), None),
            children: &[ChildMachine {
                declared: Some("plain"),
                definition: PASS_CHILD,
                // The state machine the processor is written in is STANDARD, which is
                // what its children run as when the processor names no type of its own.
                machine_type: "STANDARD",
            }],
        },
        Row {
            case: "a processor written unsorted, indented and carrying a Comment",
            definition: concat!(
                "{\n  \"StartAt\": \"M\",\n  \"States\": {\n    \"M\": {\n",
                "      \"Type\": \"Map\",\n      \"Items\": [1, 2],\n",
                "      \"Label\": \"written\",\n      \"ItemProcessor\": {\n",
                "        \"States\": { \"P\": { \"Type\": \"Pass\", \"End\": true } },\n",
                "        \"Comment\": \"the processor\",\n",
                "        \"ProcessorConfig\": { \"Mode\": \"DISTRIBUTED\", \"ExecutionType\": \"EXPRESS\" },\n",
                "        \"StartAt\": \"P\"\n      },\n      \"End\": true\n    }\n  }\n}"
            )
            .to_string(),
            children: &[ChildMachine {
                declared: Some("written"),
                // The members the processor keeps are reported in the order the
                // definition wrote them, with only the whitespace between the tokens
                // dropped.
                definition: concat!(
                    r#"{"States":{"P":{"Type":"Pass","End":true}},"#,
                    r#""Comment":"the processor","StartAt":"P"}"#
                ),
                machine_type: "EXPRESS",
            }],
        },
        Row {
            case: "a Distributed Map inside a Parallel branch",
            definition: format!(
                r#"{{"StartAt":"Fan","States":{{"Fan":{{"Type":"Parallel","Branches":[{{"StartAt":"InBranch","States":{{"InBranch":{{"Type":"Map","Items":[1,2],"ItemProcessor":{},"End":true}}}}}}],"End":true}}}}}}"#,
                processor(Some("EXPRESS"))
            ),
            children: &[ChildMachine {
                declared: None,
                definition: PASS_CHILD,
                machine_type: "EXPRESS",
            }],
        },
        Row {
            case: "a Distributed Map nested inside a Distributed Map",
            definition: format!(
                r#"{{"StartAt":"Outer","States":{{"Outer":{{"Type":"Map","Items":[1,2],"Label":"outer","ItemProcessor":{{"ProcessorConfig":{{"Mode":"DISTRIBUTED","ExecutionType":"STANDARD"}},"StartAt":"Inner","States":{{"Inner":{{"Type":"Map","Items":[1],"ItemProcessor":{},"End":true}}}}}},"End":true}}}}}}"#,
                processor(Some("EXPRESS"))
            ),
            children: &[
                ChildMachine {
                    declared: Some("outer"),
                    definition: concat!(
                        r#"{"StartAt":"Inner","States":{"Inner":{"Type":"Map","Items":[1],"#,
                        r#""ItemProcessor":{"ProcessorConfig":{"Mode":"DISTRIBUTED","#,
                        r#""ExecutionType":"EXPRESS"},"StartAt":"P","States":{"P":{"Type":"Pass","#,
                        r#""End":true}}},"End":true}}}"#
                    ),
                    machine_type: "STANDARD",
                },
                ChildMachine {
                    declared: None,
                    definition: PASS_CHILD,
                    machine_type: "EXPRESS",
                },
            ],
        },
    ]
}

/// The `mapRuns` of one execution, which fit in one page for every row here.
async fn map_runs(emulator: &Emulator, execution_arn: &str) -> Vec<Value> {
    let listed = emulator
        .ok("ListMapRuns", &json!({ "executionArn": execution_arn }))
        .await;
    listed
        .get("mapRuns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("expected a mapRuns array in {listed:?}"))
}

/// The Map Runs `execution_arn` started, once its run has left `RUNNING` and reported at
/// least `wanted` of them: looked for again until that holds or [`PATIENCE`] passes, so
/// what a case waits for is the CONDITION rather than a span of the host's clock.
async fn map_runs_once_settled(
    emulator: &Emulator,
    case: &str,
    execution_arn: &str,
    wanted: usize,
) -> Vec<Value> {
    let began = Instant::now();
    loop {
        let described = emulator
            .ok(
                "DescribeExecution",
                &json!({ "executionArn": execution_arn }),
            )
            .await;
        let listed = map_runs(emulator, execution_arn).await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING")
            && listed.len() >= wanted
        {
            return listed;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the run to settle having started at least {wanted} \
             Map Run(s) within {PATIENCE:?}, got {described:?} and {listed:?}"
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every Map Run reports the labeled ARN of the child state machine it executed, the
/// labeled ARN describes that child state machine, and every child workflow execution
/// the Map Run started reports the same ARN as the state machine that ran it.
#[tokio::test]
async fn a_map_run_reports_the_labeled_child_state_machine_it_executed() {
    let emulator = Emulator::new();
    let rows = rows();

    // Every row is started first and read afterwards, so each row's own wait finds the
    // outcomes the clock owed all of them.
    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let name = format!("child-machine-{index}");
        let machine = create_machine(&emulator, &name, &row.definition).await;
        let execution = start_execution(&emulator, &machine, "only", TWO_ITEMS).await;
        started.push((name, machine, execution));
    }

    for (row, (name, machine, execution)) in rows.iter().zip(&started) {
        let case = row.case;
        let listed =
            map_runs_once_settled(&emulator, case, execution, row.children.len()).await;
        assert!(
            !listed.is_empty(),
            "case {case:?}: expected the Distributed Map states to start Map Runs, got \
             {listed:?}"
        );

        // Rule 1: one label per Map Run, carried by BOTH its own ARN and the ARN of the
        // state machine it executed.
        let mut labels: Vec<String> = Vec::new();
        for map_run in &listed {
            let map_run_arn = string(map_run, "mapRunArn");
            let label = map_run_label(case, &map_run_arn);
            let prefix = format!("arn:aws:states:{REGION}:{ACCOUNT}:mapRun:{name}/{label}:");
            let id = map_run_arn.strip_prefix(&prefix).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a Map Run ARN under {prefix:?}, got \
                     {map_run_arn:?}"
                )
            });
            assert!(
                !id.is_empty(),
                "case {case:?}: expected a non-empty Map Run id on {map_run_arn:?}"
            );
            assert_eq!(
                string(map_run, "stateMachineArn"),
                format!("{machine}/{label}"),
                "case {case:?}: a Map Run names the labeled child state machine it \
                 executed, got {map_run:?}"
            );
            if !labels.contains(&label) {
                labels.push(label);
            }
        }

        assert_eq!(
            labels.len(),
            row.children.len(),
            "case {case:?}: expected {} child state machine(s), got the labels {labels:?}",
            row.children.len()
        );

        for (expected, label) in row.children.iter().zip(&labels) {
            match expected.declared {
                Some(declared) => assert_eq!(
                    label, declared,
                    "case {case:?}: the label of a state that declares one is that \
                     Label, got {label:?}"
                ),
                None => assert!(
                    is_generated_label(label),
                    "case {case:?}: expected a generated label for a state that \
                     declares none, got {label:?}"
                ),
            }

            // Rule 2: the labeled ARN describes the child state machine.
            let labeled = format!("{machine}/{label}");
            let described = emulator
                .ok("DescribeStateMachine", &json!({ "stateMachineArn": labeled }))
                .await;
            assert_eq!(
                (
                    described.get("stateMachineArn").and_then(Value::as_str),
                    described.get("name").and_then(Value::as_str),
                    described.get("definition").and_then(Value::as_str),
                    described.get("type").and_then(Value::as_str),
                    described.get("label").and_then(Value::as_str),
                    described.get("roleArn").and_then(Value::as_str),
                ),
                (
                    Some(labeled.as_str()),
                    Some(name.as_str()),
                    Some(expected.definition),
                    Some(expected.machine_type),
                    Some(label.as_str()),
                    Some(ROLE_ARN),
                ),
                "case {case:?}: the labeled ARN describes the Map state's own child \
                 state machine, got {described:?}"
            );
        }

        // Rule 1, the other half: every child workflow execution the first Map Run
        // started reports that Map Run's own labeled state machine, through both reads.
        let first = &listed[0];
        let map_run_arn = string(first, "mapRunArn");
        let executed = string(first, "stateMachineArn");
        let children = emulator
            .ok("ListExecutions", &json!({ "mapRunArn": &map_run_arn }))
            .await;
        let children = children
            .get("executions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected executions in {children:?}"));
        assert!(
            !children.is_empty(),
            "case {case:?}: expected the Map Run to have started children, got \
             {children:?}"
        );
        for child in &children {
            assert_eq!(
                string(child, "stateMachineArn"),
                executed,
                "case {case:?}: a listed child names the state machine that ran it, got \
                 {child:?}"
            );
            let described = emulator
                .ok(
                    "DescribeExecution",
                    &json!({ "executionArn": string(child, "executionArn") }),
                )
                .await;
            assert_eq!(
                string(&described, "stateMachineArn"),
                executed,
                "case {case:?}: the describe of a child names the state machine that \
                 executed it, got {described:?}"
            );
        }
    }
}

/// Rule 3: a labeled ARN resolves only where a DISTRIBUTED Map state of the definition
/// carries that label. The rows differ along what the label names: nothing at all, a
/// state that is not a `Map`, an INLINE `Map` state (which runs its items inside the
/// execution rather than on a child state machine), and a label of another state
/// machine's Map state.
#[tokio::test]
async fn a_labeled_arn_that_names_no_distributed_map_state_resolves_to_nothing() {
    let emulator = Emulator::new();
    let definition = json!({
        "StartAt": "Inline",
        "States": {
            "Inline": {
                "Type": "Map",
                "Items": [1],
                "Label": "inline",
                "ItemProcessor": {
                    "StartAt": "P",
                    "States": { "P": { "Type": "Pass", "End": true } },
                },
                "Next": "Done",
            },
            "Done": { "Type": "Pass", "End": true },
        },
    })
    .to_string();
    let machine = create_machine(&emulator, "labeled-refusals", &definition).await;
    // A second state machine whose Distributed Map carries a label of its own: a label
    // resolves inside the state machine the ARN names and nowhere else.
    let other_definition = top_level(Some("elsewhere"), Some("EXPRESS"));
    let other = create_machine(&emulator, "labeled-elsewhere", &other_definition).await;

    let cases = [
        ("a label no state carries", format!("{machine}/nothing-here")),
        ("a state that is not a Map", format!("{machine}/Done")),
        ("an inline Map state's own label", format!("{machine}/inline")),
        (
            "a label of another state machine's Map state",
            format!("{machine}/elsewhere"),
        ),
    ];

    for (case, arn) in &cases {
        let (status, body) = emulator
            .call("DescribeStateMachine", &json!({ "stateMachineArn": arn }))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "StateMachineDoesNotExist"),
            "case {case:?}: expected the state machine's own absent condition, got \
             {status} {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(format!("State Machine Does Not Exist: '{arn}'").as_str()),
            "case {case:?}: expected the refusal to name the ARN the request carried, \
             got {body:?}"
        );
    }

    // The positive control: the label the OTHER state machine does carry resolves
    // against that state machine.
    let described = emulator
        .ok(
            "DescribeStateMachine",
            &json!({ "stateMachineArn": format!("{other}/elsewhere") }),
        )
        .await;
    assert_eq!(
        (
            described.get("definition").and_then(Value::as_str),
            described.get("label").and_then(Value::as_str),
        ),
        (Some(PASS_CHILD), Some("elsewhere")),
        "the labeled ARN of the state machine that carries the label resolves, got \
         {described:?}"
    );
}

/// A METADATA-ONLY read of a labeled ARN withholds the child state machine's definition
/// exactly as it withholds the state machine's own, and still reports the label.
#[tokio::test]
async fn a_metadata_only_read_of_a_labeled_arn_withholds_the_child_definition() {
    let emulator = Emulator::new();
    let machine = create_machine(
        &emulator,
        "labeled-metadata-only",
        &top_level(Some("quiet"), Some("EXPRESS")),
    )
    .await;
    let labeled = format!("{machine}/quiet");

    let described = emulator
        .ok(
            "DescribeStateMachine",
            &json!({ "stateMachineArn": &labeled, "includedData": METADATA_ONLY }),
        )
        .await;
    assert_eq!(
        (
            described.get("definition").and_then(Value::as_str),
            described.get("label").and_then(Value::as_str),
            described.get("type").and_then(Value::as_str),
        ),
        (
            Some(METADATA_ONLY_DEFINITION),
            Some("quiet"),
            Some("EXPRESS")
        ),
        "a metadata-only read withholds the definition and reports the rest, got \
         {described:?}"
    );
    assert!(
        described
            .get("variableReferences")
            .is_none_or(Value::is_null),
        "a metadata-only read reports nothing read out of the definition, got \
         {described:?}"
    );
}
