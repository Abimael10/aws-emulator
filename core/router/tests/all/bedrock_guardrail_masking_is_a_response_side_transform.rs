// SPDX-License-Identifier: CC0-1.0

//! Which SIDE of an invocation a sensitive-information policy entry is evaluated on, from the
//! guardrail the Amazon Bedrock control plane created to the verdict the Bedrock Runtime data
//! plane answers.
//!
//! RULE: a sensitive-information entry — a PII entity type or a regular expression — takes its
//! own `action` on a side that names none of its own
//! (`smithy_query(query="shape",shape="GuardrailPiiEntityConfig,GuardrailRegexConfig")`: both
//! carry `action` `@required` beside the optional `inputAction` / `outputAction` /
//! `inputEnabled` / `outputEnabled`), with ONE asymmetry: masking REWRITES the text the
//! guardrail answers with ("ANONYMIZE – Mask the content and replace it with identifier tags"),
//! which is a transform of the RESPONSE, so an entry whose action is that masking one is a
//! response-side entry and the REQUEST side of it is evaluated only where the configuration
//! asks for a request-side evaluation of its own. An entry no side of an evaluation holds is
//! not evaluated there, so that side's assessment reports nothing of it and its policy reads
//! none of the text units it is charged for ("When disabled, you aren't charged for the
//! evaluation. The evaluation doesn't appear in the response.") — while a policy that DID run
//! reads its units whether or not it placed anything, which is what makes zero units mean
//! "never ran".
//!
//! Two crates have to agree for that to hold — the control plane owns the guardrail resource
//! and projects each revision onto the policies the data plane evaluates, and the data plane
//! applies them — so the suite lives here: a crate-local test can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: what the entry's action is (mask,
//! block, report only), which side the content is submitted on, whether the configuration
//! states a side of its own (an action by name, an evaluation enabled or disabled), whether the
//! entry is an entity type or a regular expression, which entity type it is, and whether the
//! submitted text carries anything the entry places.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// The signing name the whole Amazon Bedrock family shares; the request's path and host are
/// what decide which member of it claims a request.
const BEDROCK: &str = "bedrock";

/// The host a data-plane request addresses, which is how AWS separates the two endpoints.
const RUNTIME_HOST: &str = "bedrock-runtime";

/// What the guardrails here answer content they blocked on each side with.
const BLOCKED_INPUT: &str = "Sorry, the model cannot answer this question.";
const BLOCKED_OUTPUTS: &str = "Sorry, the model cannot answer with this text.";

/// One sentence carrying an email address, and one carrying a telephone number: two entity
/// types of the modelled catalogue (`GuardrailPiiEntityType` — `EMAIL`, `PHONE`), so the rule
/// is asserted on an entity type the issue did not cite as well as on the one it did.
const EMAIL_TEXT: &str = "Write to dana.reed@example.com about the order.";
const EMAIL_VALUE: &str = "dana.reed@example.com";
const PHONE_TEXT: &str = "Reach the desk on 206-555-0147 before noon.";
const PHONE_VALUE: &str = "206-555-0147";

/// A sentence no entry configured below places anything in.
const BENIGN_TEXT: &str = "What time does the library open?";

/// The regular expression one row configures, and a sentence its pattern matches.
const REGEX_NAME: &str = "order-id";
const REGEX_PATTERN: &str = "ORD-[0-9]{4}";
const REGEX_TEXT: &str = "your order is ORD-1234 today";

/// `GuardrailAction` — what the guardrail did to the content it was applied to.
const INTERVENED: &str = "GUARDRAIL_INTERVENED";
const NO_ACTION: &str = "NONE";

/// The three `actionReason` sentences, one per thing an evaluation can do.
const REASON_BLOCKED: &str = "Guardrail blocked.";
const REASON_MASKED: &str = "Guardrail masked.";
const REASON_NONE: &str = "No action.";

/// How long a poll of a guardrail's status waits before it concludes the create is stuck:
/// generous next to the span the emulator makes such a transition due after, room for a loaded
/// machine rather than for another order of magnitude.
const READY_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a poll waits between two reads.
const READY_STEP: std::time::Duration = std::time::Duration::from_millis(50);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names the family, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{BEDROCK}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One restJson1 exchange with one of the two services.
async fn call(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let mut request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("{host}.{REGION}.amazonaws.com"))
        .header("authorization", authorization());
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let bytes = body.map_or_else(Bytes::new, |body| {
        Bytes::from(serde_json::to_vec(body).expect("a serializable test body"))
    });
    let response = router
        .dispatch(request.body(bytes).expect("a well-formed request"))
        .await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `CreateGuardrail` with one `sensitiveInformationPolicyConfig`, answering the guardrail id.
async fn create_guardrail(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    policy: &Value,
) -> String {
    let (status, created) = call(
        router,
        BEDROCK,
        "POST",
        "/guardrails",
        Some(&json!({
            "name": name,
            "blockedInputMessaging": BLOCKED_INPUT,
            "blockedOutputsMessaging": BLOCKED_OUTPUTS,
            "sensitiveInformationPolicyConfig": policy,
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: the guardrail must be created, got {status} / {created}"
    );
    created["guardrailId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create answers a guardrailId, got {created}"))
        .to_string()
}

/// `ApplyGuardrail` on one revision, over the data plane's own wire.
async fn apply_guardrail(
    router: &MultiServiceRouter,
    identifier: &str,
    version: &str,
    source: &str,
    text: &str,
) -> (u16, Value) {
    call(
        router,
        RUNTIME_HOST,
        "POST",
        &format!("/guardrail/{identifier}/version/{version}/apply"),
        Some(&json!({
            "source": source,
            "content": [{ "text": { "text": text } }],
        })),
    )
    .await
}

/// The text the guardrail answered with, block by block.
fn answered(applied: &Value) -> Vec<String> {
    applied["outputs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|output| output["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// What one row expects of the one `piiEntities` entry the assessment reports, where it reports
/// one at all: the entity type that placed it, the action the evaluation TOOK and the value it
/// matched (`GuardrailPiiEntityFilter{match, type, action, detected}`).
struct PlacedEntity {
    r#type: &'static str,
    action: &'static str,
    r#match: &'static str,
}

/// One row: the sensitive-information policy a guardrail is created with, the submission it is
/// applied to, and what the apply must answer.
struct Row {
    case: &'static str,
    /// The guardrail's `name`, unique per row.
    name: &'static str,
    /// `sensitiveInformationPolicyConfig`, as the create request carries it.
    policy: Value,
    /// `GuardrailContentSource` — which side the submission is.
    source: &'static str,
    text: &'static str,
    action: &'static str,
    reason: &'static str,
    /// The text the guardrail answers with, block by block.
    outputs: Vec<String>,
    /// The entity the `sensitiveInformationPolicy` assessment must report, where it reports
    /// one; `None` where the assessment must not be reported at all.
    placed: Option<PlacedEntity>,
    /// The regular expression the assessment must report, where it reports one: its name.
    placed_regex: Option<&'static str>,
    /// `usage.sensitiveInformationPolicyUnits` — the text units the sensitive-information
    /// policy PROCESSED, which is zero where it did not run on this side. It is zero for a
    /// policy configuring regular expressions alone too: their filters are charged as the
    /// FREE units below.
    units: i64,
    /// `usage.sensitiveInformationPolicyFreeUnits` — charged for what the regular expressions
    /// placed.
    free_units: i64,
}

/// The entity-type entry `type`/`action` names, carrying whatever `extra` members the row
/// states for its sides.
fn pii_policy(entity_type: &str, action: &str, extra: Value) -> Value {
    let mut entry = json!({ "type": entity_type, "action": action });
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        entry[member] = value;
    }
    json!({ "piiEntitiesConfig": [entry] })
}

/// The regular-expression entry `action` names.
fn regex_policy(action: &str) -> Value {
    json!({
        "regexesConfig": [{
            "name": REGEX_NAME,
            "pattern": REGEX_PATTERN,
            "action": action,
        }],
    })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an entity type configured to mask, on the response side, the value present",
            name: "mask-email-output",
            policy: pii_policy("EMAIL", "ANONYMIZE", json!({})),
            source: "OUTPUT",
            text: EMAIL_TEXT,
            action: INTERVENED,
            reason: REASON_MASKED,
            outputs: vec!["Write to {EMAIL} about the order.".to_string()],
            placed: Some(PlacedEntity {
                r#type: "EMAIL",
                action: "ANONYMIZED",
                r#match: EMAIL_VALUE,
            }),
            placed_regex: None,
            units: 1,
            free_units: 0,
        },
        Row {
            case: "the same entity type and text, on the request side",
            name: "mask-email-input",
            policy: pii_policy("EMAIL", "ANONYMIZE", json!({})),
            source: "INPUT",
            text: EMAIL_TEXT,
            action: NO_ACTION,
            reason: REASON_NONE,
            outputs: Vec::new(),
            placed: None,
            placed_regex: None,
            units: 0,
            free_units: 0,
        },
        Row {
            case: "a masking entity type on the response side, text carrying nothing of it",
            name: "mask-email-output-benign",
            policy: pii_policy("EMAIL", "ANONYMIZE", json!({})),
            source: "OUTPUT",
            text: BENIGN_TEXT,
            action: NO_ACTION,
            reason: REASON_NONE,
            outputs: Vec::new(),
            placed: None,
            placed_regex: None,
            // The policy RAN and placed nothing, which is what it is charged for.
            units: 1,
            free_units: 0,
        },
        Row {
            case: "a masking entity type on the request side, text carrying nothing of it",
            name: "mask-email-input-benign",
            policy: pii_policy("EMAIL", "ANONYMIZE", json!({})),
            source: "INPUT",
            text: BENIGN_TEXT,
            action: NO_ACTION,
            reason: REASON_NONE,
            outputs: Vec::new(),
            placed: None,
            placed_regex: None,
            units: 0,
            free_units: 0,
        },
        Row {
            case: "another entity type configured to mask, on the response side",
            name: "mask-phone-output",
            policy: pii_policy("PHONE", "ANONYMIZE", json!({})),
            source: "OUTPUT",
            text: PHONE_TEXT,
            action: INTERVENED,
            reason: REASON_MASKED,
            outputs: vec!["Reach the desk on {PHONE} before noon.".to_string()],
            placed: Some(PlacedEntity {
                r#type: "PHONE",
                action: "ANONYMIZED",
                r#match: PHONE_VALUE,
            }),
            placed_regex: None,
            units: 1,
            free_units: 0,
        },
        Row {
            case: "that same entity type and text, on the request side",
            name: "mask-phone-input",
            policy: pii_policy("PHONE", "ANONYMIZE", json!({})),
            source: "INPUT",
            text: PHONE_TEXT,
            action: NO_ACTION,
            reason: REASON_NONE,
            outputs: Vec::new(),
            placed: None,
            placed_regex: None,
            units: 0,
            free_units: 0,
        },
        Row {
            case: "an entity type configured to block, on the request side",
            name: "block-email-input",
            policy: pii_policy("EMAIL", "BLOCK", json!({})),
            source: "INPUT",
            text: EMAIL_TEXT,
            action: INTERVENED,
            reason: REASON_BLOCKED,
            outputs: vec![BLOCKED_INPUT.to_string()],
            placed: Some(PlacedEntity {
                r#type: "EMAIL",
                action: "BLOCKED",
                r#match: EMAIL_VALUE,
            }),
            placed_regex: None,
            units: 1,
            free_units: 0,
        },
        Row {
            case: "an entity type configured to report only, on the request side",
            name: "report-email-input",
            policy: pii_policy("EMAIL", "NONE", json!({})),
            source: "INPUT",
            text: EMAIL_TEXT,
            action: NO_ACTION,
            reason: REASON_NONE,
            outputs: Vec::new(),
            placed: Some(PlacedEntity {
                r#type: "EMAIL",
                action: "NONE",
                r#match: EMAIL_VALUE,
            }),
            placed_regex: None,
            units: 1,
            free_units: 0,
        },
        Row {
            case: "a masking entity type whose request side names an action of its own",
            name: "mask-email-input-action-block",
            policy: pii_policy("EMAIL", "ANONYMIZE", json!({ "inputAction": "BLOCK" })),
            source: "INPUT",
            text: EMAIL_TEXT,
            action: INTERVENED,
            reason: REASON_BLOCKED,
            outputs: vec![BLOCKED_INPUT.to_string()],
            placed: Some(PlacedEntity {
                r#type: "EMAIL",
                action: "BLOCKED",
                r#match: EMAIL_VALUE,
            }),
            placed_regex: None,
            units: 1,
            free_units: 0,
        },
        Row {
            case: "a masking entity type whose request side asks for masking by name",
            name: "mask-email-input-action-anonymize",
            policy: pii_policy("EMAIL", "ANONYMIZE", json!({ "inputAction": "ANONYMIZE" })),
            source: "INPUT",
            text: EMAIL_TEXT,
            action: INTERVENED,
            reason: REASON_MASKED,
            outputs: vec!["Write to {EMAIL} about the order.".to_string()],
            placed: Some(PlacedEntity {
                r#type: "EMAIL",
                action: "ANONYMIZED",
                r#match: EMAIL_VALUE,
            }),
            placed_regex: None,
            units: 1,
            free_units: 0,
        },
        Row {
            case: "a masking entity type whose request-side evaluation is enabled by name",
            name: "mask-email-input-enabled",
            policy: pii_policy("EMAIL", "ANONYMIZE", json!({ "inputEnabled": true })),
            source: "INPUT",
            text: EMAIL_TEXT,
            action: INTERVENED,
            reason: REASON_MASKED,
            outputs: vec!["Write to {EMAIL} about the order.".to_string()],
            placed: Some(PlacedEntity {
                r#type: "EMAIL",
                action: "ANONYMIZED",
                r#match: EMAIL_VALUE,
            }),
            placed_regex: None,
            units: 1,
            free_units: 0,
        },
        Row {
            case: "a masking entity type whose response-side evaluation is turned off",
            name: "mask-email-output-disabled",
            policy: pii_policy("EMAIL", "ANONYMIZE", json!({ "outputEnabled": false })),
            source: "OUTPUT",
            text: EMAIL_TEXT,
            action: NO_ACTION,
            reason: REASON_NONE,
            outputs: Vec::new(),
            placed: None,
            placed_regex: None,
            units: 0,
            free_units: 0,
        },
        Row {
            case: "a regular expression configured to mask, on the response side",
            name: "mask-regex-output",
            policy: regex_policy("ANONYMIZE"),
            source: "OUTPUT",
            text: REGEX_TEXT,
            action: INTERVENED,
            reason: REASON_MASKED,
            outputs: vec![format!("your order is {{{REGEX_NAME}}} today")],
            placed: None,
            placed_regex: Some(REGEX_NAME),
            // The regular-expression filters of a sensitive-information policy are charged
            // as its FREE units, and are charged for what they PLACED: a policy configuring
            // no entity type reads no chargeable unit.
            units: 0,
            free_units: 1,
        },
        Row {
            case: "that same regular expression and text, on the request side",
            name: "mask-regex-input",
            policy: regex_policy("ANONYMIZE"),
            source: "INPUT",
            text: REGEX_TEXT,
            action: NO_ACTION,
            reason: REASON_NONE,
            outputs: Vec::new(),
            placed: None,
            placed_regex: None,
            units: 0,
            free_units: 0,
        },
        Row {
            case: "a regular expression configured to block, on the request side",
            name: "block-regex-input",
            policy: regex_policy("BLOCK"),
            source: "INPUT",
            text: REGEX_TEXT,
            action: INTERVENED,
            reason: REASON_BLOCKED,
            outputs: vec![BLOCKED_INPUT.to_string()],
            placed: None,
            placed_regex: Some(REGEX_NAME),
            units: 0,
            free_units: 1,
        },
    ]
}

/// RULE: a sensitive-information entry configured to MASK is a response-side transform — it is
/// evaluated, charged and reported on the response side alone unless the configuration asks for
/// a request-side evaluation of its own — while an entry that blocks or only reports is
/// evaluated on both sides, and a policy that ran reads its text units whether or not it placed
/// anything.
#[tokio::test]
async fn a_masking_sensitive_information_entry_is_evaluated_on_the_response_side_only() {
    let router = emulator();
    for row in rows() {
        let case = row.case;
        let id = create_guardrail(&router, case, row.name, &row.policy).await;
        let (status, applied) =
            apply_guardrail(&router, &id, "DRAFT", row.source, row.text).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the apply must be served, got {status} / {applied}"
        );

        let actual_action = applied["action"].as_str().unwrap_or_default();
        assert_eq!(
            actual_action, row.action,
            "case {case:?}: expected action {:?}, got {actual_action:?} in {applied}",
            row.action
        );
        let actual_reason = applied["actionReason"].as_str().unwrap_or_default();
        assert_eq!(
            actual_reason, row.reason,
            "case {case:?}: expected actionReason {:?}, got {actual_reason:?} in {applied}",
            row.reason
        );
        let actual_outputs = answered(&applied);
        assert_eq!(
            actual_outputs, row.outputs,
            "case {case:?}: expected the guardrail to answer {:?}, got {actual_outputs:?}",
            row.outputs
        );

        let assessed = &applied["assessments"][0]["sensitiveInformationPolicy"];
        if row.placed.is_none() && row.placed_regex.is_none() {
            assert!(
                assessed.is_null(),
                "case {case:?}: an evaluation that placed nothing of this policy reports none \
                 of it, got {applied}"
            );
        }
        match &row.placed {
            Some(expected) => {
                let entries = assessed["piiEntities"].as_array().cloned().unwrap_or_default();
                assert_eq!(
                    entries.len(),
                    1,
                    "case {case:?}: the value in the text is placed once, got {applied}"
                );
                let entry = &entries[0];
                assert_eq!(
                    entry["type"].as_str(),
                    Some(expected.r#type),
                    "case {case:?}: the placement names the entity type that placed it, got \
                     {entry}"
                );
                assert_eq!(
                    entry["action"].as_str(),
                    Some(expected.action),
                    "case {case:?}: the placement reports the action TAKEN, got {entry}"
                );
                assert_eq!(
                    entry["match"].as_str(),
                    Some(expected.r#match),
                    "case {case:?}: the placement reports the value it matched, got {entry}"
                );
                assert_eq!(
                    entry["detected"].as_bool(),
                    Some(true),
                    "case {case:?}: a placed entry is reported as detected, got {entry}"
                );
            }
            None => assert!(
                assessed["piiEntities"].is_null()
                    || assessed["piiEntities"]
                        .as_array()
                        .is_some_and(|entries| entries.is_empty()),
                "case {case:?}: no entity type was placed on this side, got {applied}"
            ),
        }
        match row.placed_regex {
            Some(name) => assert_eq!(
                assessed["regexes"][0]["name"].as_str(),
                Some(name),
                "case {case:?}: the placement names the regular expression that placed it, got \
                 {applied}"
            ),
            None => assert!(
                assessed["regexes"].is_null()
                    || assessed["regexes"]
                        .as_array()
                        .is_some_and(|entries| entries.is_empty()),
                "case {case:?}: no regular expression was placed on this side, got {applied}"
            ),
        }

        let usage = &applied["usage"];
        assert_eq!(
            usage["sensitiveInformationPolicyUnits"].as_i64(),
            Some(row.units),
            "case {case:?}: expected {} sensitive-information units, got {usage}",
            row.units
        );
        assert_eq!(
            usage["sensitiveInformationPolicyFreeUnits"].as_i64(),
            Some(row.free_units),
            "case {case:?}: expected {} free sensitive-information units, got {usage}",
            row.free_units
        );
    }
}

/// Poll `GetGuardrail` until the revision `version` of the guardrail reports `READY`, the way a
/// client waits for a published version before applying it.
async fn poll_until_ready(router: &MultiServiceRouter, identifier: &str, version: &str) {
    let started = std::time::Instant::now();
    loop {
        let (status, read) = call(
            router,
            BEDROCK,
            "GET",
            &format!("/guardrails/{identifier}?guardrailVersion={version}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "the guardrail must be readable, got {status} / {read}"
        );
        if read["status"].as_str() == Some("READY") {
            return;
        }
        assert!(
            started.elapsed() < READY_BUDGET,
            "a created guardrail version must become READY inside {READY_BUDGET:?}, still \
             reports {read}"
        );
        ::tokio::time::sleep(READY_STEP).await;
    }
}

/// RULE: the asymmetry belongs to the revision, so a numbered version cut from a draft that
/// configures a masking entity type is applied the same way the draft is — masking on the
/// response side, nothing on the request side.
#[tokio::test]
async fn a_published_version_masks_the_response_side_and_leaves_the_request_side_alone() {
    let router = emulator();
    let id = create_guardrail(
        &router,
        "a published version",
        "mask-email-published",
        &pii_policy("EMAIL", "ANONYMIZE", json!({})),
    )
    .await;
    let (status, cut) = call(
        &router,
        BEDROCK,
        "POST",
        &format!("/guardrails/{id}"),
        Some(&json!({ "description": "the masking policy as created" })),
    )
    .await;
    assert_eq!(
        status, 202,
        "the version must be cut, got {status} / {cut}"
    );
    let version = cut["version"]
        .as_str()
        .expect("the cut answers the version it minted")
        .to_string();
    poll_until_ready(&router, &id, &version).await;

    // (case, the side applied, the action answered, the reason, the text answered with)
    let rows: [(&str, &str, &str, &str, Vec<String>); 2] = [
        (
            "the published version on the response side",
            "OUTPUT",
            INTERVENED,
            REASON_MASKED,
            vec!["Write to {EMAIL} about the order.".to_string()],
        ),
        (
            "the published version on the request side",
            "INPUT",
            NO_ACTION,
            REASON_NONE,
            Vec::new(),
        ),
    ];
    for (case, source, action, reason, outputs) in rows {
        let (status, applied) =
            apply_guardrail(&router, &id, &version, source, EMAIL_TEXT).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the apply must be served, got {status} / {applied}"
        );
        let actual_action = applied["action"].as_str().unwrap_or_default();
        assert_eq!(
            actual_action, action,
            "case {case:?}: expected action {action:?}, got {actual_action:?} in {applied}"
        );
        let actual_reason = applied["actionReason"].as_str().unwrap_or_default();
        assert_eq!(
            actual_reason, reason,
            "case {case:?}: expected actionReason {reason:?}, got {actual_reason:?} in {applied}"
        );
        let actual_outputs = answered(&applied);
        assert_eq!(
            actual_outputs, outputs,
            "case {case:?}: expected the guardrail to answer {outputs:?}, got {actual_outputs:?}"
        );
    }
}
