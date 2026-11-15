// SPDX-License-Identifier: CC0-1.0

//! WITHDRAWING the account's enforced guardrail configuration, asserted on each invocation
//! surface the inference path serves it on: `DeleteEnforcedGuardrailConfiguration` over
//! HTTP, with the Bedrock Runtime data plane mounted beside this control plane on the one
//! shared engine the emulator boots them on.
//!
//! # The rule
//!
//! The account's enforced guardrail configuration IS the server-side enforcement the
//! inference path applies, so the delete of that configuration — addressed by the one
//! `configId` the account holds — WITHDRAWS it: an invocation made afterwards is guarded by
//! nothing, on every surface and in every response envelope, and the account's listing
//! reports no configuration. A delete addressed to a `configId` the account does not hold —
//! an id nothing minted, and any id at all once the configuration is gone — addresses no
//! configuration, is refused 404 `ResourceNotFoundException` in the wording Amazon Bedrock
//! words that class with for this resource, and leaves both the configuration and the
//! enforcement it put in force exactly as they were.
//!
//! # What the table below generalises over
//!
//! The rows differ along WHICH surface the guarded invocation reaches the data plane on and
//! which response envelope reports the guardrail's verdict — the `InvokeModel` envelopes of
//! three model families, where the enforcement is reported in the service-injected body
//! member `amazon-bedrock-guardrailAction`, and a `Converse` turn, which reports it in the
//! modelled `StopReason` instead. The sibling suite `enforced_guardrail_inference` drives
//! the other axis of the same rule (which models a configuration's `modelEnforcement`
//! covers) on one surface; this one holds the coverage fixed — a configuration carrying no
//! `modelEnforcement` covers every model — and varies the surface.
//!
//! The delete itself is driven along the `configId` it addresses: two ids the account does
//! not hold, the installed one, and the installed one once more after it is gone.

use serde_json::{json, Value};

use crate::support::{emulator_with_inference, Emulator};

/// The action member a guarded invocation's body carries, and the value it reports an
/// intervention with, read from the data plane's own publication rule rather than written
/// out here, so this suite and that service cannot disagree about either. The Converse
/// surfaces report the same verdict in the modelled stop reason.
use ::service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state::enforced_guardrail::{
    ACTION_INTERVENED, GUARDRAIL_ACTION_FIELD, STOP_REASON_GUARDRAIL_INTERVENED,
};

/// The URI the put and the listing are bound to; the delete is bound to it followed by the
/// `configId` it addresses.
const CONFIGURATION_URI: &str = "/enforcedGuardrailsConfiguration";

/// The word the fixture's guardrail blocks: a token nothing in a request or a served
/// completion carries otherwise.
const BANNED: &str = "zephyrine";

/// What the fixture's guardrail answers a blocked input with, and a blocked response with.
const BLOCKED_INPUT: &str = "This request was blocked by the account guardrail.";
const BLOCKED_OUTPUT: &str = "This response was blocked by the account guardrail.";

/// The `ResourceNotFoundException` message a request addressing a `configId` the account
/// holds no configuration under is refused with — the one sentence for that class, which
/// carries no part of the request.
const NOT_FOUND: &str =
    "No default guardrail configuration found for the specified config identifier.";

/// One surface a guarded request reaches the data plane on.
///
/// Model ids are from "Amazon Bedrock base model IDs (on-demand throughput)" and the
/// request/response envelopes from "Inference request parameters and response fields for
/// foundation models", both in the Amazon Bedrock User Guide.
struct Surface {
    case: &'static str,
    /// The model the request names, which a configuration carrying no `modelEnforcement`
    /// covers like every other.
    model_id: &'static str,
    /// The last segment of the model-addressed path this surface is bound to.
    action: &'static str,
    /// The request body, in this family's envelope, carrying [`BANNED`] in its prompt and
    /// naming NO guardrail of its own.
    body: Value,
    /// Where the answered text sits in this surface's response envelope.
    text_pointer: &'static str,
    /// Whether this surface reports the guardrail's verdict in the modelled `StopReason`
    /// (the Converse API) rather than in the injected action member (the invoke API).
    verdict_in_stop_reason: bool,
}

fn surfaces() -> Vec<Surface> {
    let prompt = format!("Tell me about {BANNED} in one line.");
    vec![
        Surface {
            case: "InvokeModel, the Nova envelope",
            model_id: "amazon.nova-lite-v1:0",
            action: "invoke",
            body: json!({ "messages": [{ "role": "user", "content": [{ "text": prompt }] }] }),
            text_pointer: "/output/message/content/0/text",
            verdict_in_stop_reason: false,
        },
        Surface {
            case: "InvokeModel, the Titan Text envelope",
            model_id: "amazon.titan-text-premier-v1:0",
            action: "invoke",
            body: json!({
                "inputText": format!("Write a haiku about {}.", BANNED.to_uppercase()),
                "textGenerationConfig": { "maxTokenCount": 512 },
            }),
            text_pointer: "/results/0/outputText",
            verdict_in_stop_reason: false,
        },
        Surface {
            case: "InvokeModel, the Anthropic envelope",
            model_id: "anthropic.claude-3-haiku-20240307-v1:0",
            action: "invoke",
            body: json!({
                "anthropic_version": "bedrock-2023-05-31",
                "max_tokens": 256,
                "messages": [{
                    "role": "user",
                    "content": [{ "type": "text", "text": format!("What is {BANNED}?") }],
                }],
            }),
            text_pointer: "/content/0/text",
            verdict_in_stop_reason: false,
        },
        Surface {
            case: "Converse, the one turn",
            model_id: "amazon.nova-pro-v1:0",
            action: "converse",
            body: json!({
                "messages": [{ "role": "user", "content": [{ "text": format!("Explain {BANNED}.") }] }],
            }),
            text_pointer: "/output/message/content/0/text",
            verdict_in_stop_reason: true,
        },
    ]
}

/// A guardrail whose word policy blocks [`BANNED`], with one numbered version: the id and
/// the version an enforced configuration names (`GuardrailNumericalVersion`, never the
/// DRAFT).
async fn guardrail_blocking_the_word(emulator: &Emulator, name: &str) -> (String, String) {
    let created = emulator
        .create(json!({
            "name": name,
            "blockedInputMessaging": BLOCKED_INPUT,
            "blockedOutputsMessaging": BLOCKED_OUTPUT,
            "wordPolicyConfig": { "wordsConfig": [{ "text": BANNED }] },
        }))
        .await;
    assert_eq!(
        created.status, 202,
        "case {name}: CreateGuardrail must answer 202, got {} {:?}",
        created.status, created.body
    );
    let id = created.text("guardrailId");
    let cut = emulator.create_version(&id, json!({})).await;
    assert_eq!(
        cut.status, 202,
        "case {name}: CreateGuardrailVersion must answer 202, got {} {:?}",
        cut.status, cut.body
    );
    (id, cut.text("version"))
}

/// `PUT /enforcedGuardrailsConfiguration` for `id` at `version`, carrying no
/// `modelEnforcement` at all — a configuration that covers every model — and answering the
/// `configId` it installed.
async fn install(emulator: &Emulator, case: &str, id: &str, version: &str) -> String {
    let installed = emulator
        .call(
            "PUT",
            CONFIGURATION_URI,
            Some(json!({
                "guardrailInferenceConfig": {
                    "guardrailIdentifier": id,
                    "guardrailVersion": version,
                },
            })),
        )
        .await;
    assert_eq!(
        installed.status, 200,
        "case {case:?}: the put must answer 200, got {} {:?}",
        installed.status, installed.body
    );
    installed.text("configId")
}

/// The configurations the account's listing reports — at most one, since the listing is
/// modelled `@length(min: 0, max: 1)`.
async fn listed(emulator: &Emulator, case: &str) -> Vec<Value> {
    let reply = emulator.call("GET", CONFIGURATION_URI, None).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the listing must answer 200, got {} {:?}",
        reply.status, reply.body
    );
    reply
        .get("guardrailsConfig")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Make the invocation this surface is, and answer whether the account-enforced guardrail
/// INTERVENED in it.
///
/// The invocation itself is served either way: an enforced guardrail replaces what the
/// request is answered with, it does not refuse the call. Which is also why the verdict is
/// cross-checked against the answered text — the guardrail's blocked messaging for an
/// intervention, and the model's own turn otherwise.
async fn guardrail_intervened(emulator: &Emulator, surface: &Surface) -> bool {
    let case = surface.case;
    let reply = emulator
        .call(
            "POST",
            &format!("/model/{}/{}", surface.model_id, surface.action),
            Some(surface.body.clone()),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the invocation must be served, got {} {:?}",
        reply.status, reply.body
    );
    let reported = if surface.verdict_in_stop_reason {
        reply.body.get("stopReason").and_then(Value::as_str)
    } else {
        reply.body.get(GUARDRAIL_ACTION_FIELD).and_then(Value::as_str)
    }
    .unwrap_or_default()
    .to_string();
    let intervened = if surface.verdict_in_stop_reason {
        reported == STOP_REASON_GUARDRAIL_INTERVENED
    } else {
        reported == ACTION_INTERVENED
    };

    let text = reply
        .body
        .pointer(surface.text_pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: no answered text at {} in {:?}",
                surface.text_pointer, reply.body
            )
        });
    if intervened {
        assert_eq!(
            text, BLOCKED_INPUT,
            "case {case:?}: a blocked invocation must answer the guardrail's \
             blockedInputMessaging, got {text:?}"
        );
    } else {
        // Read case-insensitively: a row may carry the word in any casing (a word policy
        // blocks it however it is capitalised), so an echoed prompt must not slip through
        // because it came back in another one.
        assert!(
            text != BLOCKED_INPUT
                && text != BLOCKED_OUTPUT
                && !text.to_lowercase().contains(BANNED),
            "case {case:?}: an invocation reported as {reported:?} must answer the model's own \
             turn, got {text:?}"
        );
    }
    intervened
}

/// The RULE, driven on ONE surface: with nothing installed the account enforces nothing; the
/// put puts the guardrail in force on that surface; a delete addressing a `configId` the
/// account does not hold is refused 404 and leaves it in force; the delete of the installed
/// `configId` withdraws it, so the same invocation is served the model's own turn again and
/// the listing reports no configuration; and the repeated delete is refused the same way.
///
/// One test per surface calls this, so a surface that regresses is named by the failing test
/// rather than hiding the surfaces after it.
async fn the_delete_withdraws_the_enforcement_on(surface: &Surface, fixture: &str) {
    let case = surface.case;
    // One account per surface: an account holds a single configuration, so each one installs
    // and withdraws its own.
    let emulator = emulator_with_inference();
    // `GuardrailName` is `[0-9a-zA-Z-_]+`, so the fixture is named by the caller; the
    // surface's own prose stays in the assertion messages.
    let (id, version) = guardrail_blocking_the_word(&emulator, fixture).await;

    // With nothing installed the account enforces nothing, which is the state the delete has
    // to restore.
    assert!(
        !guardrail_intervened(&emulator, surface).await,
        "case {case:?}: an account that installed no configuration enforces nothing"
    );

    let config_id = install(&emulator, case, &id, &version).await;
    assert!(
        guardrail_intervened(&emulator, surface).await,
        "case {case:?}: the installed configuration must be enforced on the invocation"
    );

    // The ids a refused delete is driven along differ in what they are: one nothing minted,
    // and the installed one with a digit appended — both inside
    // `AccountEnforcedGuardrailConfigurationId`'s published pattern (`^[a-z0-9]+$`).
    for addressed in ["zzzzzzzzzzzz".to_string(), format!("{config_id}0")] {
        let refused = emulator
            .call("DELETE", &format!("{CONFIGURATION_URI}/{addressed}"), None)
            .await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case:?}: {addressed:?} addresses no configuration, so the delete must be \
             refused 404 ResourceNotFoundException, got {} {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.get("message"),
            json!(NOT_FOUND),
            "case {case:?}: the refusal must carry the wording of that class, got {:?}",
            refused.body
        );
        assert_eq!(
            listed(&emulator, case).await.len(),
            1,
            "case {case:?}: a refused delete must leave the configuration installed"
        );
        assert!(
            guardrail_intervened(&emulator, surface).await,
            "case {case:?}: a refused delete must leave the enforcement in force"
        );
    }

    let removed = emulator
        .call("DELETE", &format!("{CONFIGURATION_URI}/{config_id}"), None)
        .await;
    assert_eq!(
        removed.status, 200,
        "case {case:?}: the installed configuration must be deletable, got {} {:?}",
        removed.status, removed.body
    );
    assert_eq!(
        listed(&emulator, case).await,
        Vec::<Value>::new(),
        "case {case:?}: the account holds no configuration once it is deleted"
    );
    assert!(
        !guardrail_intervened(&emulator, surface).await,
        "case {case:?}: the delete must withdraw the enforcement from inference"
    );

    // The configuration is gone, so the id that addressed it addresses nothing, and the
    // repeated delete is refused in the same sentence.
    let again = emulator
        .call("DELETE", &format!("{CONFIGURATION_URI}/{config_id}"), None)
        .await;
    assert_eq!(
        (again.status, again.error.as_deref(), again.get("message")),
        (404, Some("ResourceNotFoundException"), json!(NOT_FOUND)),
        "case {case:?}: a repeated delete addresses nothing, got {} {:?}",
        again.status,
        again.body
    );
}

/// The surface at `index` of the table, with the fixture name that row's guardrail takes.
fn surface(index: usize) -> (Surface, String) {
    let mut rows = surfaces();
    assert!(
        index < rows.len(),
        "case {index}: the surface table holds {} rows",
        rows.len()
    );
    (rows.remove(index), format!("withdrawn-surface-{index}"))
}

#[tokio::test]
async fn the_delete_withdraws_the_enforcement_from_the_nova_invoke_envelope() {
    let (row, fixture) = surface(0);
    the_delete_withdraws_the_enforcement_on(&row, &fixture).await;
}

#[tokio::test]
async fn the_delete_withdraws_the_enforcement_from_the_titan_invoke_envelope() {
    let (row, fixture) = surface(1);
    the_delete_withdraws_the_enforcement_on(&row, &fixture).await;
}

#[tokio::test]
async fn the_delete_withdraws_the_enforcement_from_the_anthropic_invoke_envelope() {
    let (row, fixture) = surface(2);
    the_delete_withdraws_the_enforcement_on(&row, &fixture).await;
}

#[tokio::test]
async fn the_delete_withdraws_the_enforcement_from_a_converse_turn() {
    let (row, fixture) = surface(3);
    the_delete_withdraws_the_enforcement_on(&row, &fixture).await;
}
