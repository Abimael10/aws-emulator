// SPDX-License-Identifier: CC0-1.0

//! `InvokeActStep`: taking the next step of one STORED act.
//!
//! The rule these cases assert is not "this one request answers these bytes". An act is
//! driven by its CLIENT, so a step is a hand-over in both directions and each direction has
//! a rule:
//!
//! * what a step HANDS BACK comes from what the act declared — `CreateAct` stores "a list of
//!   tool specifications that the act can invoke to complete its task", so a step asks for
//!   the first of those tools no earlier step has asked for, under a freshly minted `callId`
//!   and carrying an input instantiated from that tool's own declared input schema
//!   (`smithy_query(query="shape",shape="ToolSpec")`), and once every declared tool has been
//!   asked for a step hands back no calls at all and the act has finished;
//! * what a step ACCEPTS back is matched to what it handed out — `Call$callId` is "a unique
//!   identifier for this tool call, used to match results back to requests" and
//!   `CallResult$callId` "the identifier of the tool call that this result corresponds to" —
//!   so an identifier the request carries for something the SERVICE minted (a `callId`, the
//!   `previousStepId` a step follows) must name one this act produced, and one it never
//!   produced is refused `ValidationException` with reason `FieldValidationFailed` naming the
//!   member that carried it.
//!
//! So the tables below vary what those rules generalise over: how many tools the act
//! declares, the form of the schema each declares its input with, the form of the
//! unresolvable identifier and where in the `callResults` list it sits, which step of the act
//! it is sent on, the state the act and its run are in, and which link of the resource chain
//! is missing. The recorded "a result naming an unmatched callId" case is one row among them.

use serde_json::{json, Value};

use crate::act_status_update::{chain, move_to, new_act, summary, Chain, FINISHED_STATUSES};
use crate::workflow_lifecycle::{is_lowercase_uuid, text, Emulator};
use crate::workflow_run_status_update::move_to as move_run_to;

/// The definition each case builds its chain under.
const DEFINITION: &str = "act-step-invocation";

/// A well-formed `UuidString`
/// (`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`) no record was ever
/// minted for.
const ABSENT_UUID: &str = "00000000-0000-4000-8000-000000000000";

/// The status an act waiting for its caller to execute the calls it asked for holds, and the
/// status `CreateAct` stores a new act in.
const WAITING_ON_CLIENT: &str = "PENDING_CLIENT_ACTION";

/// The status an act that has run to the end holds.
const FINISHED: &str = "SUCCEEDED";

/// The sentence this service states for a request whose input it cannot use: the message a
/// `ValidationException` of that class carries. Spelled out here rather than taken from the
/// crate, so a change to the handler's wording cannot move both sides of the comparison at
/// once.
const INVALID_INPUT: &str = "The provided input is invalid or malformed.";

/// The `InvokeActStep` path: every identifier is an `@httpLabel` and the URI ends in a
/// trailing slash (`smithy_query(query="operation",operation="InvokeActStep")`).
fn step_path(definition: &str, run_id: &str, session_id: &str, act_id: &str) -> String {
    format!(
        "/workflow-definitions/{definition}/workflow-runs/{run_id}/sessions/{session_id}\
         /acts/{act_id}/invoke-step/"
    )
}

/// One `InvokeActStep` call carrying `body`.
async fn invoke(emulator: &Emulator, chain: &Chain, act_id: &str, body: Value) -> (u16, Value) {
    emulator
        .call(
            "PUT",
            &step_path(&chain.definition, &chain.run_id, &chain.session_id, act_id),
            Some(body),
        )
        .await
}

/// One `InvokeActStep` call carrying `results` and no `previousStepId`.
async fn step_with(
    emulator: &Emulator,
    chain: &Chain,
    act_id: &str,
    results: Vec<Value>,
) -> (u16, Value) {
    invoke(emulator, chain, act_id, json!({ "callResults": results })).await
}

/// A `CallResult` naming no call: what a caller carries on a step before any call has been
/// handed out. `CallResult$callId` is optional and `content` is a `CallResultContents` of
/// 1..100 `CallResultContent` unions, whose one member is `text`
/// (`smithy_query(query="closure",operation="InvokeActStep")`).
fn unattributed_result(content: &str) -> Value {
    json!({ "content": [{ "text": content }] })
}

/// A `CallResult` answering the call `call_id` names.
fn result_for(call_id: &str, content: &str) -> Value {
    json!({ "callId": call_id, "content": [{ "text": content }] })
}

/// One `ToolSpec` as the model shapes it: a name, a description and an input schema, all
/// three required (`smithy_query(query="shape",shape="ToolSpec")`).
fn tool_spec(name: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": format!("what {name} does"),
        "inputSchema": { "json": schema },
    })
}

/// One new act declaring `tools`, in the status `CreateAct` stores a new act in.
async fn act_declaring(
    emulator: &Emulator,
    chain: &Chain,
    task: &str,
    tools: Vec<Value>,
) -> String {
    let (status, body) = emulator
        .call(
            "PUT",
            &format!(
                "/workflow-definitions/{}/workflow-runs/{}/sessions/{}/acts",
                chain.definition, chain.run_id, chain.session_id
            ),
            Some(json!({ "task": task, "toolSpecs": tools })),
        )
        .await;
    assert_eq!(status, 201, "the act must be created, got {status} {body}");
    text(&body, "actId")
}

/// What one recorded step holds, read straight out of the store: an act's steps are reported
/// by no operation this model declares.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredStep {
    step_id: String,
    previous_step_id: Option<String>,
    /// The identifier of each call the step handed its caller.
    calls: Vec<String>,
    /// The call each result the step carried answered, `None` for a result naming none.
    answered: Vec<Option<String>>,
}

/// Every step the store holds for one act, oldest first.
fn stored_steps(emulator: &Emulator, chain: &Chain, act_id: &str, label: &str) -> Vec<StoredStep> {
    let store = emulator.store();
    let state = store.lock();
    let act = state
        .act(&chain.definition, &chain.run_id, &chain.session_id, act_id)
        .unwrap_or_else(|missing| panic!("case {label:?}: the act must be stored: {missing:?}"));
    act.execution
        .steps()
        .iter()
        .map(|step| StoredStep {
            step_id: step.step_id.clone(),
            previous_step_id: step.previous_step_id.clone(),
            calls: step.calls.iter().map(|call| call.call_id.clone()).collect(),
            answered: step.answered_calls.clone(),
        })
        .collect()
}

/// The one call a step handed back, as `(callId, name, input)`.
fn single_call(answer: &Value, label: &str) -> (String, String, Value) {
    let calls = answer["calls"]
        .as_array()
        .unwrap_or_else(|| panic!("case {label:?}: calls must be a list, got {answer}"));
    assert_eq!(
        calls.len(),
        1,
        "case {label:?}: expected one call, got {answer}"
    );
    (
        text(&calls[0], "callId"),
        text(&calls[0], "name"),
        calls[0]["input"].clone(),
    )
}

/// RULE: a step asks for the next tool the act declared and hands the next move to the
/// caller; once the declared tools run out a step asks for nothing and the act has finished.
#[tokio::test]
async fn a_step_asks_for_the_next_declared_tool_until_the_act_has_none_left() {
    let emulator = Emulator::new("us-east-1");
    let chain = chain(&emulator, DEFINITION).await;

    // Rows differ in how many tools the act declares, which is what the length of an act's
    // run of steps generalises over.
    let rows: [(&str, &[&str]); 4] = [
        ("no declared tool", &[]),
        ("one declared tool", &["search"]),
        ("two declared tools", &["search", "click"]),
        (
            "three declared tools",
            &["screenshot", "type_text", "submit"],
        ),
    ];

    for (label, tools) in rows {
        let specs = tools
            .iter()
            .map(|name| tool_spec(name, json!({ "type": "object" })))
            .collect();
        let act_id = act_declaring(&emulator, &chain, label, specs).await;

        let mut outstanding: Option<String> = None;
        let mut previous_step: Option<String> = None;
        let mut minted_steps: Vec<String> = Vec::new();
        let mut minted_calls: Vec<String> = Vec::new();

        for (index, expected_tool) in tools.iter().enumerate() {
            let results = vec![match &outstanding {
                Some(call_id) => result_for(call_id, "the tool ran"),
                None => unattributed_result("starting the act"),
            }];
            let mut body = json!({ "callResults": results });
            if let Some(step_id) = &previous_step {
                body["previousStepId"] = json!(step_id);
            }
            let (status, answer) = invoke(&emulator, &chain, &act_id, body).await;
            assert_eq!(
                status, 200,
                "case {label:?} step {index}: expected HTTP 200, got {status} {answer}"
            );

            let case = format!("{label} step {index}");
            let (call_id, name, _) = single_call(&answer, &case);
            assert_eq!(
                name, *expected_tool,
                "case {case:?}: expected the act's tool {expected_tool:?} to be asked for, \
                 got {answer}"
            );
            // `CallId` is len 1..100 (`smithy_query(query="shape",shape="CallId")`), and the
            // service mints a fresh one per call so a result can be matched back to it.
            assert!(
                !call_id.is_empty() && call_id.len() <= 100,
                "case {case:?}: expected a CallId of 1..100 characters, got {call_id:?}"
            );
            assert!(
                !minted_calls.contains(&call_id),
                "case {case:?}: expected a fresh callId, got the already-minted {call_id:?}"
            );

            let step_id = text(&answer, "stepId");
            assert!(
                is_lowercase_uuid(&step_id),
                "case {case:?}: expected stepId to be a UuidString, got {step_id:?}"
            );
            assert!(
                !minted_steps.contains(&step_id),
                "case {case:?}: expected a fresh stepId, got the already-minted {step_id:?}"
            );

            // A call is outstanding, so the next move belongs to the caller.
            let stored = summary(&emulator, &chain, &act_id).await;
            assert_eq!(
                stored["status"], WAITING_ON_CLIENT,
                "case {case:?}: expected the act to report {WAITING_ON_CLIENT}, got {stored}"
            );
            assert!(
                stored["endedAt"].is_null(),
                "case {case:?}: an act still under way reports no endedAt, got {stored}"
            );

            outstanding = Some(call_id.clone());
            previous_step = Some(step_id.clone());
            minted_calls.push(call_id);
            minted_steps.push(step_id);
        }

        // The step after the last declared tool: nothing left to ask for, so the act has run
        // to the end.
        let results = vec![match &outstanding {
            Some(call_id) => result_for(call_id, "the last tool ran"),
            None => unattributed_result("nothing to do"),
        }];
        let mut body = json!({ "callResults": results });
        if let Some(step_id) = &previous_step {
            body["previousStepId"] = json!(step_id);
        }
        let (status, answer) = invoke(&emulator, &chain, &act_id, body).await;
        assert_eq!(
            status, 200,
            "case {label:?}: the closing step must be accepted, got {status} {answer}"
        );
        assert_eq!(
            answer["calls"].as_array().map(Vec::len),
            Some(0),
            "case {label:?}: an act with no tool left asks for no call, got {answer}"
        );
        let stored = summary(&emulator, &chain, &act_id).await;
        assert_eq!(
            stored["status"], FINISHED,
            "case {label:?}: expected the act to report {FINISHED}, got {stored}"
        );
        assert!(
            !stored["endedAt"].is_null(),
            "case {label:?}: a finished act reports endedAt, got {stored}"
        );

        // Every step is recorded against the act: one per declared tool plus the closing
        // one, each carrying the calls it handed out and the calls it was answered for.
        let recorded = stored_steps(&emulator, &chain, &act_id, label);
        assert_eq!(
            recorded.len(),
            tools.len() + 1,
            "case {label:?}: expected {} recorded steps, got {recorded:?}",
            tools.len() + 1
        );
        let handed_out: Vec<String> = recorded
            .iter()
            .flat_map(|step| step.calls.clone())
            .collect();
        assert_eq!(
            handed_out, minted_calls,
            "case {label:?}: expected the record to hold every call handed out, \
             got {recorded:?}"
        );
        let answered: Vec<Option<String>> = recorded
            .iter()
            .flat_map(|step| step.answered.clone())
            .collect();
        let expected_answers: Vec<Option<String>> = std::iter::once(None)
            .chain(minted_calls.iter().cloned().map(Some))
            .collect();
        assert_eq!(
            answered, expected_answers,
            "case {label:?}: expected each call to be answered by the step after it, \
             got {recorded:?}"
        );
    }
}

/// RULE: the input a call carries is an instance of the input schema its tool declared — the
/// value the schema fixes for each property it declares, else the empty value of the type it
/// declares.
#[tokio::test]
async fn a_call_carries_an_instance_of_the_tools_declared_input_schema() {
    let emulator = Emulator::new("eu-west-2");
    let chain = chain(&emulator, DEFINITION).await;

    // Rows differ along the form of the declared schema. The keywords they use are JSON
    // Schema's own (`ToolSpec$inputSchema` is "The JSON schema that defines the expected
    // input format for the tool", a `ToolInputSchema` union whose `json` member carries that
    // schema), and the expectation is the instance each schema fixes.
    let rows: [(&str, Value, Value); 9] = [
        ("an object schema declaring no property", json!({ "type": "object" }), json!({})),
        (
            "a string property the schema leaves open",
            json!({ "type": "object", "properties": { "url": { "type": "string" } } }),
            json!({ "url": "" }),
        ),
        (
            "a property the schema defaults",
            json!({
                "type": "object",
                "properties": { "url": { "type": "string", "default": "about:blank" } },
            }),
            json!({ "url": "about:blank" }),
        ),
        (
            "a property the schema fixes with const",
            json!({
                "type": "object",
                "properties": { "mode": { "type": "string", "const": "readonly" } },
            }),
            json!({ "mode": "readonly" }),
        ),
        (
            "a property the schema enumerates",
            json!({
                "type": "object",
                "properties": { "direction": { "enum": ["up", "down"] } },
            }),
            json!({ "direction": "up" }),
        ),
        (
            "one property of each scalar type the schema can declare",
            json!({
                "type": "object",
                "properties": {
                    "count": { "type": "integer" },
                    "ratio": { "type": "number" },
                    "force": { "type": "boolean" },
                    "label": { "type": "string" },
                    "steps": { "type": "array" },
                    "nothing": { "type": "null" },
                },
            }),
            json!({
                "count": 0,
                "ratio": 0,
                "force": false,
                "label": "",
                "steps": [],
                "nothing": null,
            }),
        ),
        (
            "a nested object property",
            json!({
                "type": "object",
                "properties": {
                    "point": {
                        "type": "object",
                        "properties": { "x": { "type": "integer" }, "y": { "type": "integer" } },
                    },
                },
            }),
            json!({ "point": { "x": 0, "y": 0 } }),
        ),
        (
            "a schema declaring properties and no type",
            json!({ "properties": { "query": { "type": "string" } } }),
            json!({ "query": "" }),
        ),
        (
            "a schema whose input is a bare string",
            json!({ "type": "string" }),
            json!(""),
        ),
    ];

    for (label, schema, expected) in rows {
        let act_id = act_declaring(
            &emulator,
            &chain,
            label,
            vec![tool_spec("inspect", schema.clone())],
        )
        .await;
        let (status, answer) =
            step_with(&emulator, &chain, &act_id, vec![unattributed_result("go")]).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200, got {status} {answer}"
        );
        let (_, _, carried) = single_call(&answer, label);
        assert_eq!(
            carried, expected,
            "case {label:?}: expected the input {expected} for schema {schema}, got {carried}"
        );
    }
}

/// RULE: a result naming a call the act never asked for is refused, whatever form that
/// identifier takes and wherever in the list it sits, and the refusal takes no step.
#[tokio::test]
async fn a_result_naming_a_call_the_act_never_requested_is_refused() {
    let emulator = Emulator::new("us-west-2");
    let chain = chain(&emulator, DEFINITION).await;

    // An act that HAS asked for a call, so a row can send an identifier of the right shape
    // that still answers no call of the act under test.
    let other_act = act_declaring(
        &emulator,
        &chain,
        "another act's call",
        vec![tool_spec("search", json!({ "type": "object" }))],
    )
    .await;
    let (status, answer) =
        step_with(&emulator, &chain, &other_act, vec![unattributed_result("go")]).await;
    assert_eq!(
        status, 200,
        "the other act's first step must be accepted, got {status} {answer}"
    );
    let (other_call_id, _, _) = single_call(&answer, "another act's call");

    // Rows differ in the form of the unmatched identifier, whether a step has already been
    // taken on the act, and where in the `callResults` list the offending entry sits.
    // `CallId` is len 1..100, so one row sits on that upper bound. Each row is handed the
    // call this act has outstanding (where it took a step) and a call another act has
    // outstanding, so it can build a result list around either.
    type Results = Box<dyn Fn(Option<&str>, &str) -> Vec<Value>>;
    let rows: [(&str, bool, Results, &str); 6] = [
        (
            "a callId on an act that has asked for nothing",
            false,
            Box::new(|_, _| vec![result_for("call-1", "done")]),
            "callResults[0].callId",
        ),
        (
            "a callId at the longest length CallId admits",
            false,
            Box::new(|_, _| vec![result_for(&"c".repeat(100), "done")]),
            "callResults[0].callId",
        ),
        (
            "a callId in the form the service mints",
            false,
            Box::new(|_, _| vec![result_for(ABSENT_UUID, "done")]),
            "callResults[0].callId",
        ),
        (
            "the outstanding callId with its last character changed",
            true,
            Box::new(|outstanding, _| {
                let mut forged = outstanding.expect("a call is outstanding").to_string();
                forged.pop();
                forged.push('z');
                vec![result_for(&forged, "done")]
            }),
            "callResults[0].callId",
        ),
        (
            "a second result naming no call of this act",
            true,
            Box::new(|outstanding, _| {
                vec![
                    result_for(outstanding.expect("a call is outstanding"), "done"),
                    result_for("call-2", "done"),
                ]
            }),
            "callResults[1].callId",
        ),
        (
            // Well formed, outstanding somewhere, and no call of the act under test.
            "a callId minted for another act's call",
            true,
            Box::new(|_, other| vec![result_for(other, "done")]),
            "callResults[0].callId",
        ),
    ];

    for (label, take_a_step_first, results, member) in rows {
        let act_id = act_declaring(
            &emulator,
            &chain,
            label,
            vec![tool_spec("search", json!({ "type": "object" }))],
        )
        .await;
        let outstanding = if take_a_step_first {
            let (status, answer) =
                step_with(&emulator, &chain, &act_id, vec![unattributed_result("go")]).await;
            assert_eq!(
                status, 200,
                "case {label:?}: the first step must be accepted, got {status} {answer}"
            );
            let (call_id, _, _) = single_call(&answer, label);
            Some(call_id)
        } else {
            None
        };
        let before = stored_steps(&emulator, &chain, &act_id, label);
        let before_summary = summary(&emulator, &chain, &act_id).await;

        let sent = results(outstanding.as_deref(), &other_call_id);
        let (status, answer) = step_with(&emulator, &chain, &act_id, sent).await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected HTTP 400, got {status} {answer}"
        );
        assert_eq!(
            answer["__type"], "ValidationException",
            "case {label:?}: expected ValidationException, got {answer}"
        );
        // `ValidationExceptionReason` declares `FieldValidationFailed`
        // (`smithy_query(query="shape",shape="ValidationExceptionReason")`), which is what a
        // member carrying an identifier the service cannot resolve is.
        assert_eq!(
            answer["reason"], "FieldValidationFailed",
            "case {label:?}: expected reason \"FieldValidationFailed\", got {answer}"
        );
        assert_eq!(
            answer["fieldList"][0]["name"], member,
            "case {label:?}: expected fieldList to name {member:?}, got {answer}"
        );
        // The message states the CLASS of failure, as this service words an input it cannot
        // use; which member is at fault is what the fieldList entry carries.
        assert_eq!(
            answer["message"], INVALID_INPUT,
            "case {label:?}: expected the invalid-input message, got {answer}"
        );
        let finding = answer["fieldList"][0]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label:?}: the field's message must be a string in {answer}"));
        assert!(
            finding.contains(member),
            "case {label:?}: expected the finding to name {member:?}, got {finding:?}"
        );

        // The refusal took no step: the act holds what it held and still reports the same
        // status, so a caller that fixes the identifier resumes where it was.
        let after = stored_steps(&emulator, &chain, &act_id, label);
        assert_eq!(
            after, before,
            "case {label:?}: the refused step must change nothing, got {after:?}"
        );
        let after_summary = summary(&emulator, &chain, &act_id).await;
        assert_eq!(
            after_summary, before_summary,
            "case {label:?}: the refused step must leave the act as it was, \
             got {after_summary}"
        );
    }
}

/// RULE: `previousStepId`, when the request names one, must name a step taken on this act.
#[tokio::test]
async fn previous_step_id_must_name_a_step_taken_on_this_act() {
    let emulator = Emulator::new("eu-central-1");
    let chain = chain(&emulator, DEFINITION).await;

    // An act two steps in, so a row can name its first step as well as its last, and another
    // act's step id is available as a well-formed identifier of a step this act never took.
    // It declares more tools than the rows below take steps, so no row is answered by an act
    // that has meanwhile run out of tools and finished.
    let tools: Vec<Value> = ["search", "click", "submit", "scroll", "read", "close"]
        .iter()
        .map(|name| tool_spec(name, json!({ "type": "object" })))
        .collect();
    let act_id = act_declaring(&emulator, &chain, "previous step", tools.clone()).await;
    let (status, answer) =
        step_with(&emulator, &chain, &act_id, vec![unattributed_result("go")]).await;
    assert_eq!(status, 200, "the first step must be accepted, got {answer}");
    let first_step = text(&answer, "stepId");
    let (first_call, _, _) = single_call(&answer, "previous step");
    let (status, answer) = step_with(
        &emulator,
        &chain,
        &act_id,
        vec![result_for(&first_call, "done")],
    )
    .await;
    assert_eq!(status, 200, "the second step must be accepted, got {answer}");
    let second_step = text(&answer, "stepId");
    let (outstanding, _, _) = single_call(&answer, "previous step");

    let other_act = act_declaring(&emulator, &chain, "another act's step", tools).await;
    let (status, answer) =
        step_with(&emulator, &chain, &other_act, vec![unattributed_result("go")]).await;
    assert_eq!(
        status, 200,
        "the other act's step must be accepted, got {answer}"
    );
    let other_step = text(&answer, "stepId");

    // Rows differ in which step identifier the request names: none, one of the act's own
    // steps, and an identifier naming a step the act never took.
    let rows: [(&str, Option<String>, u16); 5] = [
        ("no previousStepId at all", None, 200),
        ("the step just taken", Some(second_step.clone()), 200),
        ("an earlier step of the same act", Some(first_step), 200),
        ("a UuidString no step was minted for", Some(ABSENT_UUID.to_string()), 400),
        ("a step taken on another act", Some(other_step), 400),
    ];

    for (label, previous, expected) in rows {
        let mut body = json!({ "callResults": [result_for(&outstanding, "done")] });
        if let Some(step_id) = &previous {
            body["previousStepId"] = json!(step_id);
        }
        let (status, answer) = invoke(&emulator, &chain, &act_id, body).await;
        assert_eq!(
            status, expected,
            "case {label:?}: expected HTTP {expected}, got {status} {answer}"
        );
        if expected == 400 {
            assert_eq!(
                answer["__type"], "ValidationException",
                "case {label:?}: expected ValidationException, got {answer}"
            );
            assert_eq!(
                answer["fieldList"][0]["name"], "previousStepId",
                "case {label:?}: expected fieldList to name previousStepId, got {answer}"
            );
            assert_eq!(
                answer["message"], INVALID_INPUT,
                "case {label:?}: expected the invalid-input message, got {answer}"
            );
        } else {
            // The step was taken: it reports an identifier of its own, which is what a later
            // step may name as its `previousStepId`.
            assert!(
                is_lowercase_uuid(&text(&answer, "stepId")),
                "case {label:?}: an accepted step reports a stepId, got {answer}"
            );
        }
    }
}

/// RULE: only a running run and an unfinished act have execution left for a further step;
/// anything else conflicts, and the act is left exactly as it was.
#[tokio::test]
async fn a_step_on_a_finished_act_or_a_run_that_is_not_running_conflicts() {
    let emulator = Emulator::new("us-east-1");
    let chain = chain(&emulator, DEFINITION).await;

    // An act that has finished has no execution left for a step to describe.
    for finished in FINISHED_STATUSES {
        let act_id = new_act(&emulator, &chain, &format!("already {finished}")).await;
        let (status, answer) = move_to(&emulator, &chain, &act_id, finished).await;
        assert_eq!(
            status, 200,
            "case {finished:?}: the act must finish first, got {status} {answer}"
        );
        let before = summary(&emulator, &chain, &act_id).await;

        let (status, answer) =
            step_with(&emulator, &chain, &act_id, vec![unattributed_result("go")]).await;
        assert_eq!(
            status, 409,
            "case {finished:?}: expected HTTP 409, got {status} {answer}"
        );
        assert_eq!(
            answer["__type"], "ConflictException",
            "case {finished:?}: expected ConflictException, got {answer}"
        );
        assert_eq!(
            answer["resourceType"], "act",
            "case {finished:?}: expected resourceType \"act\", got {answer}"
        );
        assert_eq!(
            answer["resourceId"], act_id,
            "case {finished:?}: expected the act's id as resourceId, got {answer}"
        );
        let after = summary(&emulator, &chain, &act_id).await;
        assert_eq!(
            after, before,
            "case {finished:?}: the refused step must change nothing, got {after}"
        );
        assert!(
            stored_steps(&emulator, &chain, &act_id, finished).is_empty(),
            "case {finished:?}: the refused step must not be recorded"
        );
    }

    // An act still under way keeps taking steps, whichever unfinished status it holds.
    for under_way in ["RUNNING", "PENDING_HUMAN_ACTION", WAITING_ON_CLIENT] {
        let act_id = act_declaring(
            &emulator,
            &chain,
            under_way,
            vec![tool_spec("search", json!({ "type": "object" }))],
        )
        .await;
        let (status, answer) = move_to(&emulator, &chain, &act_id, under_way).await;
        assert_eq!(
            status, 200,
            "case {under_way:?}: the act must take the status first, got {status} {answer}"
        );
        let (status, answer) =
            step_with(&emulator, &chain, &act_id, vec![unattributed_result("go")]).await;
        assert_eq!(
            status, 200,
            "case {under_way:?}: an unfinished act takes a step, got {status} {answer}"
        );
        single_call(&answer, under_way);
    }

    // A run that is no longer running takes the whole chain under it out of execution. Each
    // row ends its own run, so it needs a chain of its own.
    for run_status in FINISHED_STATUSES {
        let run_chain =
            crate::act_status_update::chain(&emulator, &format!("step-run-{run_status}")).await;
        let act_id = act_declaring(
            &emulator,
            &run_chain,
            run_status,
            vec![tool_spec("search", json!({ "type": "object" }))],
        )
        .await;
        let (status, answer) = move_run_to(
            &emulator,
            &run_chain.definition,
            &run_chain.run_id,
            run_status,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {run_status:?}: the run must end first, got {status} {answer}"
        );

        let (status, answer) =
            step_with(&emulator, &run_chain, &act_id, vec![unattributed_result("go")]).await;
        assert_eq!(
            status, 409,
            "case {run_status:?}: expected HTTP 409, got {status} {answer}"
        );
        assert_eq!(
            answer["resourceType"], "workflow-run",
            "case {run_status:?}: expected resourceType \"workflow-run\", got {answer}"
        );
        assert_eq!(
            answer["resourceId"], run_chain.run_id,
            "case {run_status:?}: expected the run's id as resourceId, got {answer}"
        );
    }
}

/// RULE: every link of the chain the path names is resolved before a step is taken, and the
/// first link the store does not hold is reported as the missing resource, by its ARN.
#[tokio::test]
async fn a_step_naming_a_resource_the_store_does_not_hold_is_a_404() {
    let region = "ap-southeast-1";
    let emulator = Emulator::new(region);
    let chain = chain(&emulator, DEFINITION).await;
    let act_id = new_act(&emulator, &chain, "present").await;

    let rows: [(&str, String, &str, String); 4] = [
        (
            "an absent definition",
            step_path("absent", &chain.run_id, &chain.session_id, &act_id),
            "workflow-definition",
            "workflow-definition/absent".to_string(),
        ),
        (
            "an absent run under a present definition",
            step_path(DEFINITION, ABSENT_UUID, &chain.session_id, &act_id),
            "workflow-run",
            format!("workflow-definition/{DEFINITION}/workflow-run/{ABSENT_UUID}"),
        ),
        (
            "an absent session under a present run",
            step_path(DEFINITION, &chain.run_id, ABSENT_UUID, &act_id),
            "session",
            format!(
                "workflow-definition/{DEFINITION}/workflow-run/{}/session/{ABSENT_UUID}",
                chain.run_id
            ),
        ),
        (
            "an absent act under a present session",
            step_path(DEFINITION, &chain.run_id, &chain.session_id, ABSENT_UUID),
            "act",
            format!(
                "workflow-definition/{DEFINITION}/workflow-run/{}/session/{}/act/{ABSENT_UUID}",
                chain.run_id, chain.session_id
            ),
        ),
    ];

    for (label, path, resource_type, resource_path) in rows {
        let (status, answer) = emulator
            .call(
                "PUT",
                &path,
                Some(json!({ "callResults": [unattributed_result("go")] })),
            )
            .await;
        assert_eq!(
            status, 404,
            "case {label:?}: expected HTTP 404, got {status} {answer}"
        );
        assert_eq!(
            answer["__type"], "ResourceNotFoundException",
            "case {label:?}: expected ResourceNotFoundException, got {answer}"
        );
        assert_eq!(
            answer["resourceType"], resource_type,
            "case {label:?}: expected resourceType {resource_type:?}, got {answer}"
        );
        let expected = format!(
            "arn:aws:nova-act:{region}:{}:{resource_path}",
            engine::EMULATOR_ACCOUNT_ID
        );
        assert_eq!(
            answer["resourceId"], expected,
            "case {label:?}: expected resourceId {expected:?}, got {answer}"
        );
    }

    // Nothing was written down a chain that did not resolve.
    assert!(
        stored_steps(&emulator, &chain, &act_id, "present").is_empty(),
        "the present act must hold no step"
    );
}

/// RULE: the steps one act holds are bounded by the tools it declared: the step after the
/// last declared tool finishes the act, and a finished act takes no further step, so the
/// record stops growing however often a caller calls.
#[tokio::test]
async fn the_steps_one_act_holds_are_bounded_by_the_tools_it_declared() {
    let emulator = Emulator::new("us-east-1");
    let chain = chain(&emulator, DEFINITION).await;

    // Rows differ in how many tools the act declares, which is what the bound is stated in
    // terms of: `ToolSpecs` is modelled `len ..100`
    // (`smithy_query(query="shape",shape="ToolSpecs")`), so an act holds at most one step per
    // declared tool plus the one that finishes it.
    for declared in [0usize, 1, 2] {
        let label = format!("{declared} declared tools");
        let specs = (0..declared)
            .map(|index| tool_spec(&format!("tool_{index}"), json!({ "type": "object" })))
            .collect();
        let act_id = act_declaring(&emulator, &chain, &label, specs).await;

        // Every step the act admits, each answering the call the step before it asked for.
        let mut outstanding: Option<String> = None;
        for taken in 0..=declared {
            let results = vec![match &outstanding {
                Some(call_id) => result_for(call_id, "done"),
                None => unattributed_result("go"),
            }];
            let (status, answer) = step_with(&emulator, &chain, &act_id, results).await;
            assert_eq!(
                status, 200,
                "case {label:?} step {taken}: expected HTTP 200, got {status} {answer}"
            );
            outstanding = answer["calls"]
                .as_array()
                .and_then(|calls| calls.first())
                .map(|call| text(call, "callId"));
        }

        let held = stored_steps(&emulator, &chain, &act_id, &label);
        assert_eq!(
            held.len(),
            declared + 1,
            "case {label:?}: expected one step per declared tool plus the closing one, \
             got {held:?}"
        );

        // The act has finished, so every further call is refused and records nothing.
        for attempt in 1..=3 {
            let (status, answer) =
                step_with(&emulator, &chain, &act_id, vec![unattributed_result("again")]).await;
            assert_eq!(
                status, 409,
                "case {label:?} attempt {attempt}: expected HTTP 409, got {status} {answer}"
            );
        }
        let after = stored_steps(&emulator, &chain, &act_id, &label);
        assert_eq!(
            after, held,
            "case {label:?}: a refused step must record nothing, got {after:?}"
        );
    }
}
