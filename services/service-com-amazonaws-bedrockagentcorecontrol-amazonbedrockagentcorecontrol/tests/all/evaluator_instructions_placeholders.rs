// SPDX-License-Identifier: CC0-1.0

//! The placeholders an LLM-as-a-Judge evaluator's instructions have to reference, end to end
//! over HTTP.
//!
//! An evaluator's instructions are "the evaluation instructions that guide the language model
//! in assessing agent performance"
//! (`smithy_query(query="doc", shape="LlmAsAJudgeEvaluatorConfig", member="instructions")`),
//! and the service substitutes the evaluated agent's own data into them wherever a placeholder
//! it allows for the submitted level is written. Instructions that reference NONE of those
//! placeholders would prompt the judge model with nothing about the agent, so the request is
//! refused — before any evaluator exists — with the modelled `ValidationException` (HTTP 400,
//! `x-amzn-errortype: ValidationException`,
//! `smithy_query(query="shape", shape="ValidationException")`: `message` and `reason`
//! required, `fieldList` naming the offending member).
//!
//! Every row drives the operations as a client does — one shared engine behind one router —
//! and the rows differ along what the rule generalises over: the placeholder referenced, the
//! form it is written in, the `EvaluatorLevel` the request evaluates at (each of the enum's
//! three values, `smithy_query(query="shape", shape="EvaluatorLevel")`), the
//! `EvaluatorConfig` member the request carries
//! (`smithy_query(query="shape", shape="EvaluatorConfig")`) and whether the instructions
//! arrive on a create or on an update.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// The placeholders an evaluator's instructions may reference, in the order the service lists
/// them when it refuses instructions that reference none.
///
/// Source: the sentence the service itself publishes for a refusal of this member — it names
/// the set it holds the submitted level to — asserted below for each value of
/// `EvaluatorLevel`.
const PLACEHOLDERS: [&str; 5] = [
    "available_tools",
    "context",
    "actual_tool_trajectory",
    "expected_tool_trajectory",
    "assertions",
];

/// Every value of `EvaluatorLevel`
/// (`smithy_query(query="shape", shape="EvaluatorLevel")`).
const LEVELS: [&str; 3] = ["TOOL_CALL", "TRACE", "SESSION"];

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One answer from the emulator: its status, the error shape `x-amzn-errortype` names and its
/// parsed body.
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

async fn send(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<&Value>) -> Answer {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answer {
        status: response.status().as_u16(),
        error_type,
        body: parsed,
    }
}

async fn create_evaluator(router: &MultiServiceRouter, body: &Value) -> Answer {
    send(router, "POST", "/evaluators/create", Some(body)).await
}

async fn get_evaluator(router: &MultiServiceRouter, id: &str) -> Answer {
    send(router, "GET", &format!("/evaluators/{id}"), None).await
}

async fn update_evaluator(router: &MultiServiceRouter, id: &str, body: &Value) -> Answer {
    send(router, "PUT", &format!("/evaluators/{id}"), Some(body)).await
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The error code `restJson1` puts in `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// An LLM-as-a-Judge configuration carrying `instructions`: the `EvaluatorConfig` member whose
/// instructions the rule is about.
fn llm_as_a_judge(instructions: &str) -> Value {
    json!({"llmAsAJudge": {
        "instructions": instructions,
        "ratingScale": {"numerical": [
            {"definition": "the agent failed", "value": 1.5, "label": "poor"},
            {"definition": "the agent succeeded", "value": 4.5, "label": "good"}
        ]},
        "modelConfig": {"bedrockEvaluatorModelConfig": {
            "modelId": "anthropic.claude-3-sonnet",
            "inferenceConfig": {"maxTokens": 512, "temperature": 0.5, "topP": 0.25}
        }}
    }})
}

/// A code-based configuration: a customer-managed Lambda function, which carries no
/// instructions at all.
fn code_based() -> Value {
    json!({"codeBased": {"lambdaConfig": {
        "lambdaArn": "arn:aws:lambda:us-east-1:000000000000:function:evaluatorJudge",
        "lambdaTimeoutInSeconds": 30
    }}})
}

/// A derived configuration: a base evaluator the service provides, run on the caller's own
/// model — also carrying no instructions.
fn derived() -> Value {
    json!({"derived": {
        "baseEvaluatorId": "Builtin.Correctness",
        "modelConfig": {"responsesEvaluatorModelConfig": {
            "modelId": "openai.gpt-4",
            "maxOutputTokens": 256,
            "temperature": 0.5,
            "topP": 0.75,
            "reasoning": {"effort": "low"}
        }}
    }})
}

/// The sentence the service refuses instructions with, for the level the request evaluates at:
/// the fixed text, the level as the enum spells it and the placeholder names that level
/// allows.
fn refusal_message(level: &str) -> String {
    format!(
        "Instructions must contain at least one of the allowed placeholders for {level} \
         level: {}",
        PLACEHOLDERS.join(", ")
    )
}

/// Instructions that reference a placeholder the level allows are accepted, and the evaluator
/// they configure reads back with the instructions as sent.
///
/// The rows differ in WHICH placeholder is referenced — each of the five, including the ones
/// a single refusal does not have to be about — in the FORM it is written in (alone, inside a
/// template reference, several at once, and with text on both sides) and in the LEVEL the
/// evaluator evaluates at.
#[tokio::test]
async fn instructions_that_reference_an_allowed_placeholder_are_accepted() {
    let router = router();

    // (case, the level the create evaluates at, the instructions it carries)
    let mut rows: Vec<(String, &str, String)> = Vec::new();
    for (index, placeholder) in PLACEHOLDERS.iter().enumerate() {
        // Each placeholder is tried at every level, so no row depends on one level's set.
        let level = LEVELS[index % LEVELS.len()];
        rows.push((
            format!("the {placeholder} placeholder inside a template reference"),
            level,
            format!("Rate the agent against {{{{{placeholder}}}}}."),
        ));
        rows.push((
            format!("the {placeholder} placeholder written alone"),
            LEVELS[(index + 1) % LEVELS.len()],
            placeholder.to_string(),
        ));
    }
    rows.push((
        "several placeholders at once".to_string(),
        "SESSION",
        format!(
            "Compare {} with {} given {}.",
            PLACEHOLDERS[2], PLACEHOLDERS[3], PLACEHOLDERS[1]
        ),
    ));
    rows.push((
        "a placeholder at the very end of long instructions".to_string(),
        "TRACE",
        format!("{} {}", "Judge the run carefully. ".repeat(10), PLACEHOLDERS[0]),
    ));

    for (index, (case, level, instructions)) in rows.iter().enumerate() {
        let name = format!("acceptedJudge{index}");
        let config = llm_as_a_judge(instructions);
        let created = create_evaluator(
            &router,
            &json!({
                "evaluatorName": name,
                "evaluatorConfig": config,
                "level": level
            }),
        )
        .await;
        assert_eq!(
            (created.status, text(&created.body, "status").as_str()),
            (202, "ACTIVE"),
            "case {case:?} at {level}: expected the create to be accepted, got {} ({})",
            created.status,
            created.body
        );

        let id = text(&created.body, "evaluatorId");
        let fetched = get_evaluator(&router, &id).await;
        assert_eq!(
            (fetched.status, fetched.body.get("evaluatorConfig")),
            (200, Some(&config)),
            "case {case:?} at {level}: the read reports the instructions the create \
             persisted, got {} ({})",
            fetched.status,
            fetched.body
        );
    }
}

/// Instructions that reference none of the placeholders the level allows are refused, in the
/// one sentence the service words that refusal with, and nothing is created.
///
/// The rows differ in the LEVEL — every value of the enum, so the sentence is asserted against
/// each level's own set — and in HOW the instructions miss: prose that names no placeholder,
/// a near miss that carries only part of a placeholder's name, and instructions that reference
/// a name the service does not define.
#[tokio::test]
async fn instructions_that_reference_no_placeholder_are_refused() {
    let router = router();

    // (case, the instructions the create carries)
    let misses: [(&str, &str); 4] = [
        ("prose that names no placeholder", "Rate how well the agent did."),
        (
            "a near miss carrying only part of a placeholder's name",
            "Judge the trajectory of the tools the agent used.",
        ),
        (
            "a template reference the service defines no placeholder for",
            "Rate {{agent_answer}} out of five.",
        ),
        ("instructions of a single character", "."),
    ];

    for level in LEVELS {
        for (index, (case, instructions)) in misses.iter().enumerate() {
            let name = format!("refusedJudge{level}{index}").replace('_', "");
            let body = json!({
                "evaluatorName": name,
                "evaluatorConfig": llm_as_a_judge(instructions),
                "level": level
            });
            let refused = create_evaluator(&router, &body).await;
            assert_eq!(
                (
                    refused.status,
                    error_code(&refused.body).as_str(),
                    refused.error_type.as_str()
                ),
                (400, "ValidationException", "ValidationException"),
                "case {case:?} at {level}: expected the modelled 400 ValidationException, got \
                 {} {:?} ({})",
                refused.status,
                refused.error_type,
                refused.body
            );
            assert_eq!(
                text(&refused.body, "message"),
                refusal_message(level),
                "case {case:?} at {level}: the refusal names the level and the placeholders it \
                 allows, got {}",
                refused.body
            );
            assert_eq!(
                refused.body.get("fieldList"),
                Some(&json!([{"name": "instructions", "message": refusal_message(level)}])),
                "case {case:?} at {level}: the refusal names the offending member in a \
                 single-entry fieldList, got {}",
                refused.body
            );
            assert_eq!(
                text(&refused.body, "reason"),
                "FieldValidationFailed",
                "case {case:?} at {level}: a member of the request failed validation, got {}",
                refused.body
            );

            // Nothing was created: the name the refused request asked for is still free, which
            // a create carrying instructions the level allows proves by taking it.
            let accepted = create_evaluator(
                &router,
                &json!({
                    "evaluatorName": name,
                    "evaluatorConfig": llm_as_a_judge(&format!("Rate against {}.", PLACEHOLDERS[1])),
                    "level": level
                }),
            )
            .await;
            assert_eq!(
                accepted.status, 202,
                "case {case:?} at {level}: the refused create left no evaluator holding the \
                 name, got {} ({})",
                accepted.status, accepted.body
            );
        }
    }
}

/// A configuration that carries no instructions at all is not held to the rule: the rule is
/// about the member that holds the prompt, and the other `EvaluatorConfig` members have none.
#[tokio::test]
async fn a_configuration_without_instructions_is_not_checked() {
    let router = router();

    for (index, level) in LEVELS.iter().enumerate() {
        for (case, config) in [("code-based", code_based()), ("derived", derived())] {
            let created = create_evaluator(
                &router,
                &json!({
                    "evaluatorName": format!("plainJudge{case}{index}").replace('-', ""),
                    "evaluatorConfig": config,
                    "level": level
                }),
            )
            .await;
            assert_eq!(
                (created.status, text(&created.body, "status").as_str()),
                (202, "ACTIVE"),
                "case {case:?} at {level}: a configuration carrying no instructions is \
                 accepted, got {} ({})",
                created.status,
                created.body
            );
        }
    }
}

/// An update is held to the same rule, against the configuration and the level the evaluator
/// would carry once the change applied: the request's own members where it names them, the
/// stored ones where it leaves them alone.
#[tokio::test]
async fn an_update_is_held_to_the_same_instructions_rule() {
    let router = router();
    let stored_config = llm_as_a_judge(&format!("Rate against {{{{{}}}}}.", PLACEHOLDERS[1]));
    let created = create_evaluator(
        &router,
        &json!({
            "evaluatorName": "patchedPlaceholders",
            "evaluatorConfig": stored_config,
            "level": "TRACE"
        }),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "case \"the create\": expected HTTP 202, got {} ({})",
        created.status, created.body
    );
    let id = text(&created.body, "evaluatorId");

    // (case, the update body, the level the refusal must name — or None when the update is
    //  accepted)
    let rows: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "instructions that reference nothing, at the stored level",
            json!({"evaluatorConfig": llm_as_a_judge("Rate how well the agent did.")}),
            Some("TRACE"),
        ),
        (
            "instructions that reference nothing, at a level the update names",
            json!({
                "level": "SESSION",
                "evaluatorConfig": llm_as_a_judge("Rate how well the agent did.")
            }),
            Some("SESSION"),
        ),
        (
            "a level alone, which leaves the stored instructions in place",
            json!({"level": "TOOL_CALL"}),
            None,
        ),
        (
            "instructions that reference a placeholder the level allows",
            json!({
                "evaluatorConfig": llm_as_a_judge(&format!(
                    "Weigh {} against {}.",
                    PLACEHOLDERS[0], PLACEHOLDERS[4]
                ))
            }),
            None,
        ),
        (
            "a configuration that carries no instructions",
            json!({"evaluatorConfig": code_based()}),
            None,
        ),
    ];

    for (case, update, refused_at) in rows {
        let answered = update_evaluator(&router, &id, &update).await;
        match refused_at {
            Some(level) => {
                assert_eq!(
                    (
                        answered.status,
                        error_code(&answered.body).as_str(),
                        answered.error_type.as_str()
                    ),
                    (400, "ValidationException", "ValidationException"),
                    "case {case:?}: expected the modelled 400 ValidationException, got {} {:?} \
                     ({})",
                    answered.status,
                    answered.error_type,
                    answered.body
                );
                assert_eq!(
                    text(&answered.body, "message"),
                    refusal_message(level),
                    "case {case:?}: the refusal names the level the change would evaluate at, \
                     got {}",
                    answered.body
                );
                // The refused change left the evaluator as it was.
                let fetched = get_evaluator(&router, &id).await;
                assert_eq!(
                    fetched.body.get("evaluatorConfig"),
                    Some(&stored_config),
                    "case {case:?}: the refused update left the stored configuration alone, \
                     got {}",
                    fetched.body
                );
            }
            None => assert_eq!(
                answered.status, 202,
                "case {case:?}: expected the change to be accepted, got {} ({})",
                answered.status, answered.body
            ),
        }
    }
}
