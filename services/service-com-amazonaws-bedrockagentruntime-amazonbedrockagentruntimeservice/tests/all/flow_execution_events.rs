// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `ListFlowExecutionEvents`
//! (`GET /flows/{flowIdentifier}/aliases/{flowAliasIdentifier}/executions/{executionIdentifier}/events`),
//! the listing of what happened during one flow execution.
//!
//! THE RULES UNDER TEST:
//!
//! * a listing reports the steps the run of that execution REALLY took, composed from the
//!   flow the run went through: the node its input entered at, the field of each node a value
//!   left by and arrived at, the expression and declared type that field carries, and the
//!   failure of a run that came to one;
//! * `eventType` selects the SIDE of the run that is listed — the flow's own input and output,
//!   or the inputs and outputs of the nodes it ran — and neither side ever carries the
//!   other's events;
//! * an event is reported only once it HAS occurred: what the start did carries the instant
//!   the execution started and is reported at once, while what the end of the run did carries
//!   the instant it ended and is reported only after the clock has brought the run to a
//!   terminal status;
//! * the events are reported in the order they occurred, and one page carries at most as many
//!   as the modelled `FlowExecutionEvents` list admits, whatever `maxResults` asks for, with a
//!   cursor into the rest that is bound to this execution and this event type;
//! * an execution the store holds nothing for is the modelled `ResourceNotFoundException`
//!   (404) with the fixed sentence this plane states for an unresolved flow resource,
//!   whichever form every identifier takes.
//!
//! The tables span what those rules generalise over: the shape of the flow the run went
//! through (a single hop, a fan-out, a path into a node kind nothing here runs), which side of
//! the run is listed, whether the run has finished, how many events there are against the page
//! bound, and the form the identifiers take.

use base64::Engine as _;
use serde_json::{json, Value};

use crate::agent_and_flow_invocations::{
    execution_arn, execution_not_found, fan_out_definition, flow_execution_instant, label,
    passthrough_definition, prompt_node_definition,
};
use crate::agent_memory::{form_encoded, Emulator, Reply, ACCOUNT_KEY};

/// The fixed sentence every listing of this service answers an unusable cursor with.
const INVALID_NEXT_TOKEN: &str = "Next token for pagination is invalid";

/// The most events one page carries: the modelled `FlowExecutionEvents` list is
/// `@length(max: 10)`, so a page cannot hand back more however large a `maxResults` asks for.
const MAX_EVENTS_PER_PAGE: usize = 10;

impl Emulator {
    /// One `ListFlowExecutionEvents` call over the modelled URI, with `query` appended to the
    /// required `eventType`.
    async fn list_flow_execution_events(
        &self,
        flow: &str,
        alias: &str,
        execution: &str,
        event_type: &str,
        query: &str,
    ) -> Reply {
        self.bedrock(
            ACCOUNT_KEY,
            "GET",
            &format!(
                "/flows/{}/aliases/{}/executions/{}/events?eventType={event_type}{query}",
                label(flow),
                label(alias),
                label(execution)
            ),
            None,
        )
        .await
    }

    /// A flow created, prepared and aliased from `definition`, with one execution started on
    /// it under `name`: the `(flowId, flowArn, aliasId, aliasArn)` the execution runs on.
    async fn flow_with_execution(
        &self,
        case: &str,
        definition: Value,
        name: &str,
        document: Value,
    ) -> (String, String, String, String) {
        let (flow_id, flow_arn, alias_id, alias_arn) =
            self.executable_flow(case, name, definition).await;
        let reply = self
            .start_flow_execution(&flow_id, &alias_id, "FlowInput", document, Some(name))
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case:?}: StartFlowExecution must answer 200, got {} {:?}",
            reply.status,
            reply.json(case)
        );
        (flow_id, flow_arn, alias_id, alias_arn)
    }
}

/// The kind of each event a listing answered, in the order it reported them: a
/// `FlowExecutionEvent` is a union, so each one travels as an object under the single key
/// naming the member it carries.
fn event_kinds(case: &str, body: &Value) -> Vec<String> {
    events(case, body)
        .iter()
        .map(|event| event_kind(case, event))
        .collect()
}

/// The events a listing answered.
fn events(case: &str, body: &Value) -> Vec<Value> {
    body["flowExecutionEvents"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: flowExecutionEvents must be a list, got {body:?}")
        })
        .clone()
}

/// The union member one event carries.
fn event_kind(case: &str, event: &Value) -> String {
    let object = event
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: an event must be an object, got {event:?}"));
    assert_eq!(
        object.len(),
        1,
        "case {case:?}: an event carries exactly one union member, got {event:?}"
    );
    object
        .keys()
        .next()
        .expect("the one member of the union")
        .to_string()
}

/// The body one event carries, under the member naming its kind.
fn event_body(case: &str, event: &Value) -> Value {
    event[event_kind(case, event).as_str()].clone()
}

/// The `nextToken` a listing handed out, or `""` when it handed out none.
fn next_token(case: &str, body: &Value) -> String {
    body.get("nextToken")
        .map(|token| {
            token
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: nextToken must be a string, got {body:?}"))
                .to_string()
        })
        .unwrap_or_default()
}

/// A fan-out whose input node reaches `outputs` output nodes, so one run delivers that many
/// values: the shape the page bound is tested against, built from the same node types,
/// union keys and IO data types the build-time model declares as the hand-written definitions
/// beside it use.
fn wide_fan_out_definition(outputs: usize) -> Value {
    let names: Vec<String> = (1..=outputs).map(|index| format!("Output{index:02}")).collect();
    json!({
        "nodes": std::iter::once(json!({
            "name": "FlowInput",
            "type": "Input",
            "configuration": { "input": {} },
            "outputs": [{ "name": "document", "type": "String" }],
        }))
        .chain(names.iter().map(|name| json!({
            "name": name,
            "type": "Output",
            "configuration": { "output": {} },
            "inputs": [{ "name": "document", "type": "String", "expression": "$.data" }],
        })))
        .collect::<Vec<Value>>(),
        "connections": names
            .iter()
            .map(|name| json!({
                "name": format!("InputTo{name}"),
                "type": "Data",
                "source": "FlowInput",
                "target": name,
                "configuration": { "data": { "sourceOutput": "document", "targetInput": "document" } },
            }))
            .collect::<Vec<Value>>(),
    })
}

/// RULE: a listing reports, for the side of the run `eventType` names, the steps that run
/// really took — and only the ones that have already occurred.
#[tokio::test]
async fn the_events_of_a_run_are_the_steps_it_took_on_the_side_the_request_asks_for() {
    /// One run, and the events each side of it reports before and after it finishes.
    struct Row {
        case: &'static str,
        /// The definition the alias routes to, so the rows differ in the shape of the flow
        /// the run went through.
        definition: Value,
        /// The events the flow's own side reports while the run is still going, then once it
        /// has finished.
        flow_running: Vec<&'static str>,
        flow_finished: Vec<&'static str>,
        /// The same for the side of the nodes it ran.
        node_running: Vec<&'static str>,
        node_finished: Vec<&'static str>,
        /// The nodes the finished events name, in the order they are reported.
        flow_nodes: Vec<&'static str>,
        node_nodes: Vec<&'static str>,
    }

    let rows = [
        Row {
            case: "an input wired straight to one output",
            definition: passthrough_definition(),
            flow_running: vec!["flowInputEvent"],
            flow_finished: vec!["flowInputEvent", "flowOutputEvent"],
            node_running: vec!["nodeOutputEvent"],
            node_finished: vec!["nodeOutputEvent", "nodeInputEvent"],
            flow_nodes: vec!["FlowInput", "FlowOutput"],
            node_nodes: vec!["FlowInput", "FlowOutput"],
        },
        Row {
            case: "an input fanned out to two outputs",
            definition: fan_out_definition(),
            flow_running: vec!["flowInputEvent"],
            flow_finished: vec!["flowInputEvent", "flowOutputEvent", "flowOutputEvent"],
            node_running: vec!["nodeOutputEvent"],
            node_finished: vec!["nodeOutputEvent", "nodeInputEvent", "nodeInputEvent"],
            flow_nodes: vec!["FlowInput", "LeftOutput", "RightOutput"],
            node_nodes: vec!["FlowInput", "LeftOutput", "RightOutput"],
        },
        Row {
            case: "a path into a node kind nothing here runs",
            definition: prompt_node_definition(),
            flow_running: vec!["flowInputEvent"],
            flow_finished: vec!["flowInputEvent", "flowFailureEvent"],
            node_running: vec!["nodeOutputEvent"],
            node_finished: vec!["nodeOutputEvent", "nodeFailureEvent"],
            // A flow-level failure names no node: the member the modelled `FlowFailureEvent`
            // carries is the error, not a node.
            flow_nodes: vec!["FlowInput"],
            node_nodes: vec!["FlowInput", "Rewrite"],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        let case = row.case;
        let name = format!("events-run-{index}");
        let document = json!({ "data": format!("payload {index}") });
        let (flow_id, _, alias_id, _) = emulator
            .flow_with_execution(case, row.definition.clone(), &name, document.clone())
            .await;

        // While the run is going, only what the START did is reported: an output of the flow
        // and a failure of it have not happened yet.
        for (event_type, expected) in [("Flow", &row.flow_running), ("Node", &row.node_running)] {
            let reply = emulator
                .list_flow_execution_events(&flow_id, &alias_id, &name, event_type, "")
                .await;
            assert_eq!(
                reply.status,
                200,
                "case {case:?}/{event_type} while running: expected 200, got {} {:?}",
                reply.status,
                reply.json(case)
            );
            let body = reply.json(case);
            assert_eq!(
                event_kinds(case, &body),
                expected.iter().map(|kind| kind.to_string()).collect::<Vec<String>>(),
                "case {case:?}/{event_type} while running: unexpected events, got {body:?}"
            );
            assert_eq!(
                next_token(case, &body),
                "",
                "case {case:?}/{event_type} while running: a page that fits hands out no \
                 cursor, got {body:?}"
            );
        }

        // The clock, driven rather than waited for: the run is now due to have finished.
        emulator.finish_running_executions();
        let read = emulator
            .get_flow_execution(&flow_id, &alias_id, &name)
            .await;
        let execution = read.json(case);
        let started = flow_execution_instant(case, "startedAt", &execution["startedAt"]);
        let ended = flow_execution_instant(case, "endedAt", &execution["endedAt"]);

        for (event_type, expected, expected_nodes) in [
            ("Flow", &row.flow_finished, &row.flow_nodes),
            ("Node", &row.node_finished, &row.node_nodes),
        ] {
            let reply = emulator
                .list_flow_execution_events(&flow_id, &alias_id, &name, event_type, "")
                .await;
            assert_eq!(
                reply.status,
                200,
                "case {case:?}/{event_type}: expected 200, got {} {:?}",
                reply.status,
                reply.json(case)
            );
            let body = reply.json(case);
            assert_eq!(
                event_kinds(case, &body),
                expected.iter().map(|kind| kind.to_string()).collect::<Vec<String>>(),
                "case {case:?}/{event_type}: unexpected events, got {body:?}"
            );

            let listed = events(case, &body);
            let nodes: Vec<String> = listed
                .iter()
                .filter_map(|event| {
                    event_body(case, event)
                        .get("nodeName")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect();
            assert_eq!(
                nodes,
                expected_nodes.iter().map(|node| node.to_string()).collect::<Vec<String>>(),
                "case {case:?}/{event_type}: unexpected nodes, got {body:?}"
            );

            // Every event carries the instant it occurred: the start's own, or the one the
            // run ended at. The test moved the end back to the start, so the order is
            // non-decreasing rather than strictly increasing.
            let mut previous = started;
            for (position, event) in listed.iter().enumerate() {
                let kind = event_kind(case, event);
                let timestamp = flow_execution_instant(
                    case,
                    "timestamp",
                    &event_body(case, event)["timestamp"],
                );
                let expected_instant = if position == 0 { started } else { ended };
                assert_eq!(
                    timestamp, expected_instant,
                    "case {case:?}/{event_type}: {kind} must carry the instant it occurred, \
                     got {timestamp:?} against started {started:?} and ended {ended:?}"
                );
                assert!(
                    timestamp >= previous,
                    "case {case:?}/{event_type}: {kind} is out of order, {timestamp:?} \
                     follows {previous:?}"
                );
                previous = timestamp;
            }

            // The input the flow was entered with is the document the start carried, under
            // the field the flow's own input node declares. The nodes' side reports the same
            // document through the input node's own output, with the members that side
            // declares beside it (`a_node_event_carries_the_fields_the_stored_flow_declares`).
            if event_type == "Flow" {
                let first = event_body(case, &listed[0]);
                assert_eq!(
                    first["fields"],
                    json!([{ "name": "document", "content": { "document": document.clone() } }]),
                    "case {case:?}/Flow: the flow's input is the document the start carried, \
                     got {first:?}"
                );
            }
        }

        // A failed run reports the SAME sentence through its events as the read of the
        // execution reports as the error of its run, and the codes the two planes model for
        // a failure caused by a node.
        if row.flow_finished.contains(&"flowFailureEvent") {
            let message = execution["errors"][0]["message"].as_str().unwrap_or_default();
            assert!(
                !message.is_empty(),
                "case {case:?}: a failed execution reports the error of its run, got \
                 {execution:?}"
            );
            let flow = emulator
                .list_flow_execution_events(&flow_id, &alias_id, &name, "Flow", "")
                .await
                .json(case);
            let failure = event_body(case, &events(case, &flow)[1]);
            assert_eq!(
                failure,
                json!({
                    "timestamp": execution["endedAt"],
                    "errorCode": "NODE_EXECUTION_FAILED",
                    "errorMessage": message,
                }),
                "case {case:?}: the flow's failure is the one the run came to, got {failure:?}"
            );
            let node = emulator
                .list_flow_execution_events(&flow_id, &alias_id, &name, "Node", "")
                .await
                .json(case);
            let failure = event_body(case, &events(case, &node)[1]);
            assert_eq!(
                failure,
                json!({
                    "nodeName": "Rewrite",
                    "timestamp": execution["endedAt"],
                    "errorCode": "DEPENDENCY_FAILED",
                    "errorMessage": message,
                }),
                "case {case:?}: the node's failure names the node nothing ran, got {failure:?}"
            );
        }
    }
}

/// RULE: the fields a node event carries are the ones the stored flow declares — the field
/// name, the node and output the value came from, the expression that input extracts with,
/// and the declared data type.
#[tokio::test]
async fn a_node_event_carries_the_fields_the_stored_flow_declares() {
    /// One flow, and what its definition declares about each hop out of the input node.
    struct Row {
        case: &'static str,
        definition: Value,
        /// (target node, the input of it the value arrives at, the expression that input
        /// extracts with, the declared `FlowNodeIODataType`) — read off the definition above.
        hops: Vec<(&'static str, &'static str, &'static str, &'static str)>,
        /// The declared type of the input node's own output.
        input_type: &'static str,
    }

    let rows = [
        Row {
            case: "one hop, extracting the whole document",
            definition: passthrough_definition(),
            hops: vec![("FlowOutput", "document", "$.data", "String")],
            input_type: "String",
        },
        Row {
            case: "two hops, each extracting its own branch",
            definition: fan_out_definition(),
            hops: vec![
                ("LeftOutput", "document", "$.data.left", "Object"),
                ("RightOutput", "document", "$.data.right", "Object"),
            ],
            input_type: "Object",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        let case = row.case;
        let name = format!("fields-run-{index}");
        let document = json!({ "data": { "left": index, "right": "right" } });
        let (flow_id, _, alias_id, _) = emulator
            .flow_with_execution(case, row.definition.clone(), &name, document.clone())
            .await;
        emulator.finish_running_executions();

        let body = emulator
            .list_flow_execution_events(&flow_id, &alias_id, &name, "Node", "")
            .await
            .json(case);
        let listed = events(case, &body);
        assert_eq!(
            listed.len(),
            row.hops.len() + 1,
            "case {case:?}: one output of the input node and one input per hop, got {body:?}"
        );

        // What the input node handed on, and where the flow carries it next.
        let handed_on = event_body(case, &listed[0]);
        assert_eq!(
            handed_on["fields"],
            json!([{
                "name": "document",
                "content": { "document": document.clone() },
                "next": row
                    .hops
                    .iter()
                    .map(|(node, input, _, _)| json!({ "nodeName": node, "inputFieldName": input }))
                    .collect::<Vec<Value>>(),
                "type": row.input_type,
            }]),
            "case {case:?}: the input node's output is the one its definition declares, got \
             {handed_on:?}"
        );

        for (position, (node, input, expression, data_type)) in row.hops.iter().enumerate() {
            let arrived = event_body(case, &listed[position + 1]);
            assert_eq!(
                arrived["nodeName"].as_str().unwrap_or_default(),
                *node,
                "case {case:?}: hop {position} reaches {node}, got {arrived:?}"
            );
            assert_eq!(
                arrived["fields"],
                json!([{
                    "name": input,
                    "content": { "document": document.clone() },
                    "source": {
                        "nodeName": "FlowInput",
                        "outputFieldName": "document",
                        "expression": expression,
                    },
                    "type": data_type,
                }]),
                "case {case:?}: the input {node} received is the one its definition declares, \
                 got {arrived:?}"
            );
        }
    }
}

/// RULE: one page carries at most as many events as the modelled list admits, whatever
/// `maxResults` asks for, and the cursor into the rest resumes strictly after the last event
/// already handed out and is bound to the execution and the event type that issued it.
#[tokio::test]
async fn the_events_of_a_run_are_paged_by_the_bound_the_model_declares() {
    let emulator = Emulator::new();
    let case = "a fan-out with more events than one page admits";
    // One flow-level event for the input, and one per output the run delivered: more than the
    // modelled page bound, so the listing has to split.
    let outputs = MAX_EVENTS_PER_PAGE + 2;
    let name = "paged-run";
    let document = json!({ "data": "paged" });
    let (flow_id, _, alias_id, _) = emulator
        .flow_with_execution(case, wide_fan_out_definition(outputs), name, document)
        .await;
    emulator.finish_running_executions();
    let total = outputs + 1;

    // (case, the maxResults the request asks for, the events the first page carries)
    let rows: Vec<(&str, &str, usize)> = vec![
        ("no maxResults at all", "", MAX_EVENTS_PER_PAGE),
        ("one event per page", "&maxResults=1", 1),
        ("a page of three", "&maxResults=3", 3),
        (
            "the largest maxResults the model admits",
            "&maxResults=1000",
            MAX_EVENTS_PER_PAGE,
        ),
    ];

    for (row, query, expected_page) in rows {
        // Every page of the listing, followed to the end through the cursors it hands out.
        let mut collected: Vec<String> = Vec::new();
        let mut token = String::new();
        let mut pages = 0;
        loop {
            // A cursor this service hands out is base64, whose `+`, `/` and `=` have to
            // travel percent-encoded for the server to read back the token it issued;
            // encoded through the one percent-encoder this test crate already has.
            let paged = if token.is_empty() {
                query.to_string()
            } else {
                format!("{query}&nextToken={}", form_encoded(&token))
            };
            let reply = emulator
                .list_flow_execution_events(&flow_id, &alias_id, name, "Flow", &paged)
                .await;
            assert_eq!(
                reply.status,
                200,
                "case {row:?}: expected 200, got {} {:?}",
                reply.status,
                reply.json(row)
            );
            let body = reply.json(row);
            let kinds = event_kinds(row, &body);
            if pages == 0 {
                assert_eq!(
                    kinds.len(),
                    expected_page,
                    "case {row:?}: unexpected first page size, got {body:?}"
                );
            }
            assert!(
                kinds.len() <= MAX_EVENTS_PER_PAGE,
                "case {row:?}: a page carries at most {MAX_EVENTS_PER_PAGE} events, got {}",
                kinds.len()
            );
            collected.extend(kinds);
            token = next_token(row, &body);
            pages += 1;
            if token.is_empty() {
                break;
            }
            assert!(
                pages <= total,
                "case {row:?}: the listing hands out a cursor past its own events, after \
                 {pages} pages"
            );
        }
        assert_eq!(
            collected.len(),
            total,
            "case {row:?}: every event is handed out exactly once, got {collected:?}"
        );
        assert_eq!(
            collected[0], "flowInputEvent",
            "case {row:?}: the run's input comes first, got {collected:?}"
        );
        assert!(
            collected[1..].iter().all(|kind| kind == "flowOutputEvent"),
            "case {row:?}: every later event is an output of the run, got {collected:?}"
        );
    }

    // The nodes the outputs name, in one order for one run however the pages fall.
    let body = emulator
        .list_flow_execution_events(&flow_id, &alias_id, name, "Flow", "&maxResults=1000")
        .await
        .json(case);
    let first_page: Vec<String> = events(case, &body)
        .iter()
        .filter_map(|event| {
            event_body(case, event)
                .get("nodeName")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let mut sorted = first_page.clone();
    sorted.sort();
    assert_eq!(
        first_page, sorted,
        "case {case:?}: one run reports its outputs the same way round every time, got \
         {first_page:?}"
    );

    // A cursor is only meaningful for the listing that issued it.
    let issued = next_token(case, &body);
    assert!(
        !issued.is_empty(),
        "case {case:?}: a run with more events than one page admits hands out a cursor, got \
         {body:?}"
    );
    // (case, the event type the cursor is replayed on, the cursor)
    let refused: Vec<(&str, &str, String)> = vec![
        ("a cursor of the flow's own events, on the nodes'", "Node", issued.clone()),
        ("a token this service never issued", "Flow", "not-a-cursor".to_string()),
        (
            "a token of the right shape carrying another scope",
            "Flow",
            base64::engine::general_purpose::STANDARD
                .encode("v1:ListFlowExecutionEvents:another-execution:Flow:1"),
        ),
    ];
    for (row, event_type, token) in refused {
        let reply = emulator
            .list_flow_execution_events(
                &flow_id,
                &alias_id,
                name,
                event_type,
                &format!("&nextToken={}", form_encoded(&token)),
            )
            .await;
        assert_eq!(
            (reply.error_code(row).as_str(), reply.status),
            ("ValidationException", 400),
            "case {row:?}: expected the invalid-cursor refusal, got {} {:?}",
            reply.status,
            reply.json(row)
        );
        assert_eq!(
            reply.message(row),
            INVALID_NEXT_TOKEN,
            "case {row:?}: unexpected refusal text"
        );
    }
}

/// RULE: the listing resolves the execution it addresses the way every execution-scoped
/// operation does — in either form each identifier admits — and an execution the store holds
/// nothing for is the modelled not-found.
#[tokio::test]
async fn a_listing_resolves_the_execution_it_addresses_in_every_identifier_form() {
    let emulator = Emulator::new();
    let case = "a started execution, addressed every way";
    let name = "addressed-run";
    let document = json!({ "data": "addressed" });
    let (flow_id, flow_arn, alias_id, alias_arn) = emulator
        .flow_with_execution(case, passthrough_definition(), name, document)
        .await;
    emulator.finish_running_executions();
    let run_arn = execution_arn(&flow_id, &alias_id, name);

    // (case, flowIdentifier, flowAliasIdentifier, executionIdentifier)
    let rows: Vec<(&str, String, String, String)> = vec![
        (
            "every identifier in its bare form",
            flow_id.clone(),
            alias_id.clone(),
            name.to_string(),
        ),
        (
            "the flow and the alias as ARNs",
            flow_arn.clone(),
            alias_arn.clone(),
            name.to_string(),
        ),
        (
            "the execution as its own ARN",
            flow_id.clone(),
            alias_id.clone(),
            run_arn.clone(),
        ),
    ];
    for (row, flow, alias, execution) in rows {
        let reply = emulator
            .list_flow_execution_events(&flow, &alias, &execution, "Flow", "")
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {row:?}: expected 200, got {} {:?}",
            reply.status,
            reply.json(row)
        );
        let body = reply.json(row);
        assert_eq!(
            event_kinds(row, &body),
            vec![
                "flowInputEvent".to_string(),
                "flowOutputEvent".to_string()
            ],
            "case {row:?}: every form of the same labels reports the same run, got {body:?}"
        );
    }

    // (case, flowIdentifier, flowAliasIdentifier, executionIdentifier) — every value
    // satisfies its member's own constraints, and none of them names a stored execution.
    let missing: Vec<(&str, String, String, String)> = vec![
        (
            "an execution name nothing started",
            flow_id.clone(),
            alias_id.clone(),
            "never-started".to_string(),
        ),
        (
            "an execution ARN of another alias",
            flow_id.clone(),
            alias_id.clone(),
            execution_arn(&flow_id, "ZYXWVUTSRQ", name),
        ),
        (
            "an execution of a flow nothing holds",
            "ABCDEFGHIJ".to_string(),
            alias_id.clone(),
            name.to_string(),
        ),
    ];
    for (row, flow, alias, execution) in missing {
        let reply = emulator
            .list_flow_execution_events(&flow, &alias, &execution, "Node", "")
            .await;
        assert_eq!(
            (reply.error_code(row).as_str(), reply.status),
            ("ResourceNotFoundException", 404),
            "case {row:?}: expected the not-found refusal, got {} {:?}",
            reply.status,
            reply.json(row)
        );
        assert_eq!(
            reply.message(row),
            execution_not_found(),
            "case {row:?}: unexpected refusal text"
        );
    }
}
