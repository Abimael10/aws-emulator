// SPDX-License-Identifier: CC0-1.0

//! In which ORDER `RegisterWebhookWithThirdParty` and
//! `DeregisterWebhookWithThirdParty` refuse a webhook they cannot connect, over the wire
//! the service actually speaks (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: CodePipeline_20150709.<Operation>`).
//!
//! The rule under test: both operations judge the webhook's own configuration before
//! anything its definition points at. The connection they make and break is carried by
//! the shared secret of the GitHub HMAC authentication, so a webhook configured with any
//! other `WebhookAuthenticationType` is refused on its authentication alone — the same
//! `ValidationException` sentence however its `targetPipeline` resolves — and only a
//! webhook that CAN carry the connection has its `targetPipeline` resolved against the
//! account's pipelines, which is where the dangling-reference refusal belongs. Resolving
//! the name itself still comes before both: a name no stored webhook carries is
//! `WebhookNotFoundException`, the only other error either operation models.
//!
//! What the sibling suite `third_party_webhooks` already spans — the authentication
//! types against how the target pipeline relates to the account's pipelines — is not
//! repeated here; its helpers are reused. The rows this suite adds are the ones that
//! show the order does not turn on anything else: the authentication catalog driven
//! from the model (`WEBHOOK_AUTHENTICATION_TYPE_VALUES`), the same reference resolved
//! after the refusal, and `WebhookName`'s length bounds and character classes.
//!
//! The harness is the one `pipeline_lifecycle` owns, so every assertion goes through the
//! same router the production emulator builds.

use serde_json::json;

use service_com_amazonaws_codepipeline_codepipeline_20150709::ops::types::WEBHOOK_AUTHENTICATION_TYPE_VALUES;
use service_com_amazonaws_codepipeline_codepipeline_20150709::state::webhook_authentication_unsupported_message;

use crate::pipeline_lifecycle::declaration;
use crate::third_party_webhooks::{definition_targeting, emulator};

/// The pair of operations that connects a stored webhook to the external tool: the rule
/// is one rule, so every case drives both.
const OPERATIONS: [&str; 2] = [
    "RegisterWebhookWithThirdParty",
    "DeregisterWebhookWithThirdParty",
];

/// The status both errors either operation models default to, and the codes themselves.
const REFUSAL_STATUS: u16 = 400;
const VALIDATION: &str = "ValidationException";
const NOT_FOUND: &str = "WebhookNotFoundException";

/// The authentication type whose shared secret the connection is made with — the one
/// value of `WEBHOOK_AUTHENTICATION_TYPE_VALUES` these operations serve.
const GITHUB_HMAC: &str = "GITHUB_HMAC";

/// A name no pipeline is created under until a case creates one.
const ABSENT_PIPELINE: &str = "absent-pipeline";

/// RULE: for a webhook that is not the GitHub HMAC one, the authentication refusal is
/// the whole answer — the pipeline its definition names never enters it, so the sentence
/// stands while that reference dangles and after the account comes to hold a pipeline of
/// that name, and it states the webhook the request addressed with the authentication the
/// store holds for it. Resolving the NAME still precedes it: once the webhook is gone,
/// the pair answers `WebhookNotFoundException` instead.
///
/// The rows are every value the model's `WebhookAuthenticationType` catalog
/// (`ops::types::WEBHOOK_AUTHENTICATION_TYPE_VALUES`) declares besides the served one —
/// so `IP` is covered as well as `UNAUTHENTICATED` — crossed with names at both of
/// `WebhookName`'s modelled length bounds (1..100) and over the character classes its
/// pattern (`^[A-Za-z0-9.@\-_]+$`) allows, so the rule is shown not to turn on the shape
/// of the name.
#[tokio::test]
async fn a_non_github_hmac_webhook_is_refused_on_its_authentication_alone() {
    let names = [
        ("WebhookName's minimum length", "a".to_string()),
        ("WebhookName's maximum length", "w".repeat(100)),
        (
            "every character class the pattern allows",
            "Hook.name@9-_x".to_string(),
        ),
    ];
    let unserved: Vec<&str> = WEBHOOK_AUTHENTICATION_TYPE_VALUES
        .iter()
        .copied()
        .filter(|authentication| *authentication != GITHUB_HMAC)
        .collect();
    assert!(
        !unserved.is_empty(),
        "the model's authentication catalog carries a type these operations do not \
         serve, got {WEBHOOK_AUTHENTICATION_TYPE_VALUES:?}"
    );

    for (name_case, name) in &names {
        for authentication in unserved.iter().copied() {
            let emulator = emulator().await;
            let case = format!("{name_case} carrying {authentication}");
            emulator
                .ok(
                    "PutWebhook",
                    &json!({
                        "webhook": definition_targeting(name, ABSENT_PIPELINE, authentication),
                    }),
                )
                .await;
            let expected = webhook_authentication_unsupported_message(name, authentication);
            // The wording written out once, so a reword of the handler's text fails this
            // suite rather than following it.
            assert_eq!(
                expected,
                format!(
                    "Webhook {name} must be configured with GITHUB_HMAC authentication type. \
                     Type is {authentication}."
                ),
                "case {case:?}: the refusal states the requirement the service states"
            );

            // The dangling reference first, then the same reference resolved: the answer
            // is the authentication sentence both times, because what the webhook IS is
            // judged before what it points at.
            for (reference_case, create_pipeline) in [
                ("a target pipeline nothing holds", false),
                ("the same reference, resolved", true),
            ] {
                if create_pipeline {
                    emulator
                        .ok(
                            "CreatePipeline",
                            &json!({"pipeline": declaration(ABSENT_PIPELINE)}),
                        )
                        .await;
                }
                for operation in OPERATIONS {
                    let (status, response) = emulator
                        .call(operation, &json!({"webhookName": name}))
                        .await;
                    assert_eq!(
                        (
                            status,
                            response["__type"].as_str().unwrap_or_default().to_string(),
                            response["message"].as_str().unwrap_or_default().to_string(),
                        ),
                        (REFUSAL_STATUS, VALIDATION.to_string(), expected.clone()),
                        "case {case:?} with {reference_case} through {operation}: expected \
                         {expected:?}, got {status} with {response}"
                    );
                }
            }

            // And resolving the NAME still precedes that refusal: once the webhook is
            // gone, the pair answers the error the model declares for a name no stored
            // webhook carries, which carries no message to interpolate.
            emulator.ok("DeleteWebhook", &json!({"name": name})).await;
            for operation in OPERATIONS {
                let (status, response) = emulator
                    .call(operation, &json!({"webhookName": name}))
                    .await;
                assert_eq!(
                    (
                        status,
                        response["__type"].as_str().unwrap_or_default().to_string()
                    ),
                    (REFUSAL_STATUS, NOT_FOUND.to_string()),
                    "case {case:?} through {operation}: a name no stored webhook carries is \
                     refused as not found, got {status} with {response}"
                );
            }
        }
    }
}
