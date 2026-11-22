// SPDX-License-Identifier: CC0-1.0

//! The guardrail stage of the Converse API: the revision a request NAMES is resolved
//! before the inference, and the policies of the revision it resolved to decide what the
//! turn serves.
//!
//! Three rules, each asserted over a SET of inputs:
//!
//! * a `guardrailConfig` that names a revision this account does not hold cannot be
//!   applied to the turn at all, so the request is refused `400 ValidationException`
//!   carrying the one sentence Amazon Bedrock answers an unresolvable guardrail reference
//!   with — never served unguarded — while every revision the account DOES hold is
//!   applied and served;
//! * a turn whose input a policy of the resolved revision BLOCKS does not reach the
//!   generation stage: it ends on the modelled `StopReason` `guardrail_intervened` and
//!   carries that revision's own `blockedInputMessaging`, while a turn the same revision
//!   places nothing in ends on `end_turn` carrying the turn itself;
//! * a turn an input-side DENY topic blocked is charged for NO tokens and reports the
//!   topic that blocked it as a `BLOCKED` entry of `trace.guardrail.inputAssessment`,
//!   attributed to the revision the request named.
//!
//! The rows of the first differ along WHICH part of the reference does not resolve (an
//! unheld guardrail, a version that was never cut, a version at the published digit
//! ceiling, each by the bare id and by the ARN) and along the revisions that do resolve
//! (the draft and a numbered one). The rows of the second differ along the POLICY that
//! blocks (a denied topic placed by its name, a denied topic placed by one of its sample
//! phrases, a configured word) and carry the off-topic control for each. The rows of the
//! third differ along what the topic's configuration places the prompt BY — a term of the
//! name however the name spells its words, the terms of a sample phrase, the words of the
//! definition — along the revision applied, and along how many topics the policy
//! configures; each carries its own off-topic control.
//!
//! The guardrail is published through the very function the Bedrock control plane
//! publishes one with ([`state::guardrail_registry::publish`]), so what these suites
//! apply is what a created guardrail is.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state;
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state::guardrail_registry::{
    DeniedTopic, GuardrailPolicies, PolicyAction, PublishedGuardrail, Sides, WordEntry,
    DRAFT_VERSION, TOPIC_TYPE_DENY,
};
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_runtime_inference::{post_json, Call};

/// The guardrail every row here names.
const GUARDRAIL_ID: &str = "abcd1234efgh";

/// A guardrail id no row ever publishes, so a request naming it names no guardrail this
/// account holds.
const UNHELD_GUARDRAIL_ID: &str = "ffff0000ffff";

/// What the published guardrail answers content it blocked on each side with: the
/// caller's own configured strings, which a blocked turn has to carry verbatim.
const BLOCKED_INPUT: &str = "Sorry, the model cannot answer this question.";
const BLOCKED_OUTPUT: &str = "Sorry, the model cannot answer with this text.";

/// The model every turn below is served on.
const MODEL: &str = "amazon.nova-lite-v1:0";

/// The numbered revision the account publishes beside the draft.
const PUBLISHED_VERSION: &str = "1";

/// The emulator under test, plus the engine so a row can publish the guardrail the
/// control plane would have published.
fn emulator_with_engine() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (engine, router)
}

/// The guardrail's ARN, as the control plane mints one.
fn guardrail_arn() -> String {
    engine::emulator_arn("bedrock", "us-east-1", &format!("guardrail/{GUARDRAIL_ID}"))
}

/// A revision carrying only the two blocked-messaging strings.
fn revision() -> GuardrailPolicies {
    GuardrailPolicies {
        blocked_input_messaging: BLOCKED_INPUT.to_string(),
        blocked_outputs_messaging: BLOCKED_OUTPUT.to_string(),
        ..GuardrailPolicies::default()
    }
}

/// Publish `revisions` under the guardrail, the way the control plane publishes a created
/// guardrail and each version it cuts.
fn publish(engine: &engine::InMemoryEngine, revisions: BTreeMap<String, GuardrailPolicies>) {
    assert!(
        state::guardrail_registry::publish(
            engine,
            PublishedGuardrail {
                id: GUARDRAIL_ID.to_string(),
                arn: guardrail_arn(),
                revisions,
            }
        ),
        "the guardrail must be publishable on the engine the service holds"
    );
}

/// One `Converse` turn through the guardrail `identifier` and `version` name.
async fn converse(
    router: &MultiServiceRouter,
    identifier: &str,
    version: &str,
    prompt: &str,
) -> Call {
    post_json(
        router,
        &format!("/model/{MODEL}/converse"),
        &json!({
            "messages": [{ "role": "user", "content": [{ "text": prompt }] }],
            "guardrailConfig": {
                "guardrailIdentifier": identifier,
                "guardrailVersion": version,
            },
        }),
    )
    .await
}

/// The text blocks of the assistant message a served turn answers with, concatenated.
fn text_of(case: &str, call: &Call) -> String {
    let body = call.json();
    body["output"]["message"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: the turn must carry an assistant message: {body}"))
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect()
}

// ---------------------------------------------------------------------------
// The reference a request names has to resolve
// ---------------------------------------------------------------------------

/// RULE: a Converse request whose `guardrailConfig` names a revision this account does not
/// hold is refused `400 ValidationException` with the sentence Amazon Bedrock answers an
/// unresolvable guardrail reference with, and no turn is served for it; a reference that
/// DOES resolve is applied and the turn is served.
#[tokio::test]
async fn a_converse_request_naming_an_unheld_guardrail_revision_is_refused() {
    let (engine, router) = emulator_with_engine();
    // The account holds the draft and ONE numbered version of one guardrail.
    publish(
        &engine,
        BTreeMap::from([
            (DRAFT_VERSION.to_string(), revision()),
            (PUBLISHED_VERSION.to_string(), revision()),
        ]),
    );

    // (case, the identifier, the version, resolves)
    let rows: [(&str, String, &str, bool); 8] = [
        (
            "a numeric version of the held guardrail that was never cut",
            GUARDRAIL_ID.to_string(),
            "99",
            false,
        ),
        (
            "a version at the published digit ceiling of the version pattern",
            GUARDRAIL_ID.to_string(),
            "99999999",
            false,
        ),
        (
            "the held guardrail's ARN, a version that was never cut",
            guardrail_arn(),
            "7",
            false,
        ),
        (
            "a guardrail this account does not hold, by the bare id",
            UNHELD_GUARDRAIL_ID.to_string(),
            DRAFT_VERSION,
            false,
        ),
        (
            "a guardrail this account does not hold, by an ARN",
            engine::emulator_arn(
                "bedrock",
                "us-east-1",
                &format!("guardrail/{UNHELD_GUARDRAIL_ID}"),
            ),
            PUBLISHED_VERSION,
            false,
        ),
        (
            "the draft of the held guardrail",
            GUARDRAIL_ID.to_string(),
            DRAFT_VERSION,
            true,
        ),
        (
            "the published version of the held guardrail",
            GUARDRAIL_ID.to_string(),
            PUBLISHED_VERSION,
            true,
        ),
        (
            "the published version, by the guardrail's ARN",
            guardrail_arn(),
            PUBLISHED_VERSION,
            true,
        ),
    ];

    for (case, identifier, version, resolves) in rows {
        let call = converse(&router, &identifier, version, "Name one primary colour.").await;
        if resolves {
            assert_eq!(
                call.status,
                200,
                "case {case:?}: a reference that resolves must be served, got {} / {}",
                call.status,
                String::from_utf8_lossy(&call.body)
            );
            assert_eq!(
                call.json()["stopReason"].as_str(),
                Some("end_turn"),
                "case {case:?}: expected the turn to end on its own, got {}",
                call.json()
            );
            continue;
        }
        assert_eq!(
            call.status,
            400,
            "case {case:?}: an unresolvable reference must be refused, got {} / {}",
            call.status,
            String::from_utf8_lossy(&call.body)
        );
        assert_eq!(
            call.error_code(),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            call.error_code()
        );
        assert_eq!(
            call.json()["message"].as_str(),
            Some(state::guardrail_registry::GUARDRAIL_NOT_FOUND),
            "case {case:?}: the refusal must say the reference names no revision, got {}",
            call.json()
        );
        // Nothing of a turn is served with the refusal: a request that asked to be
        // guarded is never answered unguarded.
        assert!(
            call.json()["output"].is_null() && call.json()["usage"].is_null(),
            "case {case:?}: a refused request serves no turn, got {}",
            call.json()
        );
    }
}

// ---------------------------------------------------------------------------
// A policy that blocks the input stops the turn
// ---------------------------------------------------------------------------

/// A denied topic with the name, definition and sample phrases a row configures.
fn denied_topic(name: &str, definition: &str, examples: &[&str]) -> DeniedTopic {
    DeniedTopic {
        name: name.to_string(),
        kind: TOPIC_TYPE_DENY.to_string(),
        definition: definition.to_string(),
        examples: examples.iter().map(|phrase| phrase.to_string()).collect(),
        sides: Sides::both(PolicyAction::Block),
    }
}

/// RULE: a turn whose input the resolved revision BLOCKS ends on the modelled
/// `guardrail_intervened` carrying that revision's own `blockedInputMessaging`, and the
/// model is never invoked; a turn the same revision places nothing in ends on `end_turn`
/// carrying the turn itself.
#[tokio::test]
async fn a_blocked_input_stops_the_turn_with_the_revisions_blocked_messaging() {
    // The rows differ along the policy that blocks and along what places the text in it.
    // (case, the policies, the blocked prompt, the control prompt)
    let rows: [(&str, GuardrailPolicies, &str, &str); 3] = [
        (
            "a denied topic placed by its own name",
            GuardrailPolicies {
                topics: vec![denied_topic("Fiduciary Advice", "", &[])],
                ..revision()
            },
            "Can you give me fiduciary advice?",
            "What is the capital of France?",
        ),
        (
            "a denied topic placed by one of its sample phrases",
            GuardrailPolicies {
                topics: vec![denied_topic(
                    "t-7781",
                    "",
                    &["Should I buy bitcoin as an investment this month?"],
                )],
                ..revision()
            },
            "Should I buy ethereum as an investment this month?",
            "Summarize the water cycle in one sentence.",
        ),
        (
            "a configured word the word policy blocks",
            GuardrailPolicies {
                words: vec![WordEntry {
                    text: "zephyrine".to_string(),
                    sides: Sides::both(PolicyAction::Block),
                }],
                ..revision()
            },
            "tell me about zephyrine",
            "tell me about page tokens",
        ),
    ];

    for (case, policies, blocked_prompt, control_prompt) in rows {
        let (engine, router) = emulator_with_engine();
        publish(
            &engine,
            BTreeMap::from([(DRAFT_VERSION.to_string(), policies)]),
        );

        let blocked = converse(&router, GUARDRAIL_ID, DRAFT_VERSION, blocked_prompt).await;
        assert_eq!(
            blocked.status,
            200,
            "case {case:?}: an intervened turn is still served, got {} / {}",
            blocked.status,
            String::from_utf8_lossy(&blocked.body)
        );
        assert_eq!(
            blocked.json()["stopReason"].as_str(),
            Some("guardrail_intervened"),
            "case {case:?}: expected the guardrail to intervene, got {}",
            blocked.json()
        );
        let served = text_of(case, &blocked);
        assert_eq!(
            served, BLOCKED_INPUT,
            "case {case:?}: a blocked turn carries the revision's blockedInputMessaging, got \
             {served:?}"
        );

        let control = converse(&router, GUARDRAIL_ID, DRAFT_VERSION, control_prompt).await;
        assert_eq!(
            control.status,
            200,
            "case {case:?}: the control turn must be served, got {} / {}",
            control.status,
            String::from_utf8_lossy(&control.body)
        );
        assert_eq!(
            control.json()["stopReason"].as_str(),
            Some("end_turn"),
            "case {case:?}: the control prompt must end the turn on its own, got {}",
            control.json()
        );
        let control_text = text_of(case, &control);
        assert!(
            !control_text.is_empty() && control_text != BLOCKED_INPUT,
            "case {case:?}: a turn the guardrail passed carries the turn itself, got \
             {control_text:?}"
        );
    }
}

/// RULE: the blocked messaging a turn carries is the one the GUARDRAIL the request
/// resolved to was configured with, so two guardrails configured with different messaging
/// answer their own.
#[tokio::test]
async fn the_blocked_messaging_is_the_resolved_revisions_own() {
    // (case, the configured blockedInputMessaging)
    let rows = [
        ("the suite's own sentence", BLOCKED_INPUT),
        ("a marker a caller would grep for", "GUARDRAIL-BLOCK-MARKER"),
        (
            "a sentence naming the account's policy",
            "This request was blocked by the account guardrail.",
        ),
    ];
    for (case, messaging) in rows {
        let (engine, router) = emulator_with_engine();
        publish(
            &engine,
            BTreeMap::from([(
                DRAFT_VERSION.to_string(),
                GuardrailPolicies {
                    blocked_input_messaging: messaging.to_string(),
                    blocked_outputs_messaging: messaging.to_string(),
                    words: vec![WordEntry {
                        text: "zephyrine".to_string(),
                        sides: Sides::both(PolicyAction::Block),
                    }],
                    ..GuardrailPolicies::default()
                },
            )]),
        );
        let blocked = converse(
            &router,
            GUARDRAIL_ID,
            DRAFT_VERSION,
            "tell me about zephyrine",
        )
        .await;
        let served = text_of(case, &blocked);
        assert_eq!(
            (blocked.json()["stopReason"].as_str(), served.as_str()),
            (Some("guardrail_intervened"), messaging),
            "case {case:?}: expected the guardrail's own messaging, got {}",
            blocked.json()
        );
    }
}

/// The modelled members a served Converse turn always carries, whether or not a guardrail
/// intervened in it (`smithy_query(query="operation",operation="Converse")`: `output`,
/// `stopReason`, `usage` and `metrics` are all required).
const REQUIRED_MEMBERS: [&str; 4] = ["output", "stopReason", "usage", "metrics"];

/// RULE: an intervened turn is a SERVED turn, so it answers every member the operation
/// requires — a client reading one does not have to special-case the intervention.
#[tokio::test]
async fn an_intervened_turn_answers_every_required_member() {
    let case = "an intervened turn";
    let (engine, router) = emulator_with_engine();
    publish(
        &engine,
        BTreeMap::from([(
            DRAFT_VERSION.to_string(),
            GuardrailPolicies {
                topics: vec![denied_topic("Fiduciary Advice", "", &[])],
                ..revision()
            },
        )]),
    );
    let blocked = converse(
        &router,
        GUARDRAIL_ID,
        DRAFT_VERSION,
        "Can you give me fiduciary advice?",
    )
    .await;
    let body: Value = blocked.json();
    for member in REQUIRED_MEMBERS {
        assert!(
            !body[member].is_null(),
            "case {case:?}: the output requires {member}, got {body}"
        );
    }
    assert!(
        body["usage"]["inputTokens"].as_i64().is_some(),
        "case {case:?}: the usage must report the tokens the input was, got {}",
        body["usage"]
    );
}

// ---------------------------------------------------------------------------
// An input-side DENY topic refuses the turn before the model is invoked
// ---------------------------------------------------------------------------

/// One row of the input-side topic table: the topic policy a revision configures, the
/// prompt that is on one of its topics, the control prompt that is on none of them, and
/// the topic the blocked prompt has to be reported against.
struct TopicRow {
    case: &'static str,
    /// The revision the request names, so the attribution is asserted against the
    /// revision the CALLER asked for rather than a fixed one.
    version: &'static str,
    topics: Vec<DeniedTopic>,
    blocked_prompt: &'static str,
    control_prompt: &'static str,
    /// The `name` of the topic the blocked prompt is on.
    expected_topic: &'static str,
}

/// One `Converse` turn through the guardrail, asking for the guardrail trace.
async fn converse_traced(
    router: &MultiServiceRouter,
    version: &str,
    prompt: &str,
) -> Call {
    post_json(
        router,
        &format!("/model/{MODEL}/converse"),
        &json!({
            "messages": [{ "role": "user", "content": [{ "text": prompt }] }],
            "inferenceConfig": { "maxTokens": 200, "temperature": 0.0 },
            "guardrailConfig": {
                "guardrailIdentifier": GUARDRAIL_ID,
                "guardrailVersion": version,
                "trace": "enabled",
            },
        }),
    )
    .await
}

/// The topic entries one assessment reports, as `(name, type, action, detected)`.
fn assessed_topics(assessment: &Value) -> Vec<(String, String, String, bool)> {
    assessment["topicPolicy"]["topics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|topic| {
            (
                topic["name"].as_str().unwrap_or_default().to_string(),
                topic["type"].as_str().unwrap_or_default().to_string(),
                topic["action"].as_str().unwrap_or_default().to_string(),
                topic["detected"].as_bool().unwrap_or_default(),
            )
        })
        .collect()
}

/// RULE: a guardrail's topic policy is evaluated against the request's own content
/// BEFORE the generation stage, so a prompt on a topic the revision denies is refused
/// before the model is invoked: the turn ends on `guardrail_intervened` carrying the
/// revision's `blockedInputMessaging`, reports no tokens at all, and names the topic that
/// blocked it as a `BLOCKED` entry of that guardrail's input assessment, attributed to the
/// revision the request named. A prompt on none of the revision's topics is generated for
/// as usual and no entry of its topic policy is `BLOCKED`.
#[tokio::test]
async fn an_input_side_denied_topic_refuses_the_turn_before_the_model_is_invoked() {
    // The rows differ along what the topic's own configuration places the prompt BY, the
    // revision applied, and how many topics the policy configures.
    let rows = [
        TopicRow {
            case: "a term of a hyphenated topic name, on the draft",
            version: DRAFT_VERSION,
            topics: vec![denied_topic(
                "sim2real-crypto-investment-advice",
                "Advice about buying or selling cryptocurrency.",
                &["Is now a good time to get into bitcoin?"],
            )],
            blocked_prompt: "Should I buy ethereum as an investment this month?",
            control_prompt: "Name one primary colour.",
            expected_topic: "sim2real-crypto-investment-advice",
        },
        TopicRow {
            case: "a term of a Pascal-case topic name, on a published version",
            version: PUBLISHED_VERSION,
            topics: vec![denied_topic("MedicalDosageAdvice", "", &[])],
            blocked_prompt: "How many milligrams of this medicine is a safe dosage?",
            control_prompt: "Summarize the water cycle in one sentence.",
            expected_topic: "MedicalDosageAdvice",
        },
        TopicRow {
            case: "the terms of a sample phrase, the name an opaque label",
            version: DRAFT_VERSION,
            topics: vec![denied_topic(
                "t-5512",
                "",
                &["Which shares should I pick for my pension?"],
            )],
            blocked_prompt: "Which shares do I pick?",
            control_prompt: "How tall is Mount Fuji?",
            expected_topic: "t-5512",
        },
        TopicRow {
            case: "the words of the definition, the name an opaque label",
            version: PUBLISHED_VERSION,
            topics: vec![denied_topic(
                "t-5513",
                "Any request for a recommendation about buying or selling a security.",
                &[],
            )],
            blocked_prompt: "Give me a recommendation about selling this security.",
            control_prompt: "How tall is Mount Fuji?",
            expected_topic: "t-5513",
        },
        TopicRow {
            case: "one topic of several the policy configures",
            version: DRAFT_VERSION,
            topics: vec![
                denied_topic("Fiduciary Advice", "", &[]),
                denied_topic("weapon_manufacturing", "", &[]),
                denied_topic("t-5514", "", &["Who will win the election?"]),
            ],
            blocked_prompt: "Explain how a weapon is made at home.",
            control_prompt: "What is the capital of France?",
            expected_topic: "weapon_manufacturing",
        },
    ];

    for row in rows {
        let case = row.case;
        let (engine, router) = emulator_with_engine();
        let policies = GuardrailPolicies {
            topics: row.topics.clone(),
            ..revision()
        };
        publish(
            &engine,
            BTreeMap::from([(row.version.to_string(), policies)]),
        );

        // The blocked leg: refused before generation.
        let blocked = converse_traced(&router, row.version, row.blocked_prompt).await;
        assert_eq!(
            blocked.status,
            200,
            "case {case:?}: an intervened turn is still served, got {} / {}",
            blocked.status,
            String::from_utf8_lossy(&blocked.body)
        );
        let body: Value = blocked.json();
        assert_eq!(
            body["stopReason"].as_str(),
            Some("guardrail_intervened"),
            "case {case:?}: a prompt on a denied topic must stop the turn with \
             guardrail_intervened, got {body}"
        );
        let served = text_of(case, &blocked);
        assert_eq!(
            served, BLOCKED_INPUT,
            "case {case:?}: a blocked turn carries the revision's blockedInputMessaging, got \
             {served:?}"
        );
        // The model was never invoked, so there is nothing to charge the caller for.
        for member in ["inputTokens", "outputTokens", "totalTokens"] {
            let actual = body["usage"][member].as_i64();
            assert_eq!(
                actual,
                Some(0),
                "case {case:?}: a turn refused before generation reports usage.{member}=0, got \
                 {actual:?} in {}",
                body["usage"]
            );
        }
        // The topic that blocked it, reported against the guardrail that was applied.
        let input = &body["trace"]["guardrail"]["inputAssessment"][GUARDRAIL_ID];
        assert!(
            input.is_object(),
            "case {case:?}: the input assessment must be keyed by {GUARDRAIL_ID}, got {}",
            body["trace"]
        );
        let expected_entry = (
            row.expected_topic.to_string(),
            TOPIC_TYPE_DENY.to_string(),
            "BLOCKED".to_string(),
            true,
        );
        let placed = assessed_topics(input);
        assert!(
            placed.contains(&expected_entry),
            "case {case:?}: expected the input assessment to carry {expected_entry:?}, got \
             {placed:?} in {input}"
        );
        // WHICH action the guardrail took, beside the assessment it took it on: the
        // trace reports it from the same verdict the apply surface reports it from.
        let reason = body["trace"]["guardrail"]["actionReason"].as_str();
        assert_eq!(
            reason,
            Some("Guardrail blocked."),
            "case {case:?}: a blocked turn's trace must say the content was blocked, got \
             {reason:?} in {}",
            body["trace"]
        );
        let details = &input["appliedGuardrailDetails"];
        assert_eq!(
            (
                details["guardrailId"].as_str(),
                details["guardrailVersion"].as_str()
            ),
            (Some(GUARDRAIL_ID), Some(row.version)),
            "case {case:?}: the block must be attributed to the revision the request named, \
             got {details}"
        );

        // The control leg: on none of the revision's topics, so the turn is generated for.
        let control = converse_traced(&router, row.version, row.control_prompt).await;
        let control_body: Value = control.json();
        assert_eq!(
            control_body["stopReason"].as_str(),
            Some("end_turn"),
            "case {case:?}: a prompt on none of the denied topics ends the turn on its own, \
             got {control_body}"
        );
        let control_text = text_of(case, &control);
        assert!(
            !control_text.is_empty() && control_text != BLOCKED_INPUT,
            "case {case:?}: a turn the topic policy passed carries the turn itself, got \
             {control_text:?}"
        );
        let output_tokens = control_body["usage"]["outputTokens"].as_i64();
        assert!(
            output_tokens.is_some_and(|tokens| tokens > 0),
            "case {case:?}: a turn that WAS generated for is charged for what it generated, \
             got {output_tokens:?}"
        );
        let control_input = &control_body["trace"]["guardrail"]["inputAssessment"][GUARDRAIL_ID];
        let control_reason = control_body["trace"]["guardrail"]["actionReason"].as_str();
        assert_eq!(
            control_reason,
            Some("No action."),
            "case {case:?}: a turn the guardrail acted on in nothing says so, got \
             {control_reason:?} in {}",
            control_body["trace"]
        );
        let control_placed = assessed_topics(control_input);
        assert!(
            control_placed
                .iter()
                .all(|(_, _, action, _)| action != "BLOCKED"),
            "case {case:?}: no topic blocks a prompt that is on none of them, got \
             {control_placed:?} in {control_input}"
        );
    }
}
