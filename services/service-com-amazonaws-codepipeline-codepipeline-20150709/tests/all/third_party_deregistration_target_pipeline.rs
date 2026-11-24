// SPDX-License-Identifier: CC0-1.0

//! `DeregisterWebhookWithThirdParty` (and the `RegisterWebhookWithThirdParty` that makes
//! the same connection) over the wire the service speaks (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: CodePipeline_20150709.<Operation>`).
//!
//! The rule under test: the connection either third-party webhook operation makes or
//! breaks runs to the SOURCE ACTION of the pipeline the stored
//! `WebhookDefinition$targetPipeline` names, so that reference is resolved against the
//! account's pipelines on EVERY call, and a webhook whose reference resolves to no
//! pipeline is refused with `ValidationException` (HTTP 400) naming the webhook the
//! request addressed and the pipeline its definition points at — never answered with the
//! operation's empty success, which would report a deregistration that cannot have
//! happened.
//!
//! The rows span what the rule generalises over: the operation (both halves of the
//! connection), how the reference comes to be dangling (a pipeline never created, one
//! deleted after the webhook was defined, one a later `PutWebhook` re-pointed the webhook
//! at), and names at both ends of the modelled 1..100 length of `WebhookName` and
//! `PipelineName` and over their shared pattern's character classes
//! (`smithy_query(query="shape", shape="WebhookName,PipelineName")`: length 1..100,
//! pattern `^[A-Za-z0-9.@\-_]+$`). Every webhook here carries the GITHUB_HMAC
//! authentication the connection is made with, so no row depends on how the refusal for
//! another `WebhookAuthenticationType` is ordered against this one.
//!
//! The control in each row is the same call once the account holds a pipeline of that
//! name: it is served with the empty 200, which is what distinguishes the legitimate
//! success from the refusal.
//!
//! The harness comes from `tests/all/pipeline_lifecycle.rs`, so every assertion goes
//! through the same router the production emulator builds.

use serde_json::{json, Value};

use service_com_amazonaws_codepipeline_codepipeline_20150709::state::{
    webhook_target_pipeline_absent_message, AUTHENTICATION_GITHUB_HMAC,
};

use crate::pipeline_lifecycle::{build_stage, Emulator, ROLE_ARN};

/// The two operations that resolve a stored webhook's target pipeline.
const REGISTER: &str = "RegisterWebhookWithThirdParty";
const DEREGISTER: &str = "DeregisterWebhookWithThirdParty";

/// The refusal both model for a webhook they cannot serve, with the status the model
/// defaults its client errors to.
const VALIDATION: &str = "ValidationException";
const NOT_FOUND: &str = "WebhookNotFoundException";
const REFUSAL_STATUS: u16 = 400;

/// A `SecretToken` satisfying `WebhookAuthConfigurationSecretToken`'s modelled 1..100
/// length — what a GITHUB_HMAC webhook is configured with.
const SECRET_TOKEN: &str = "s3cr3t-token";

/// The name of the source action every pipeline here declares, which the webhooks point
/// at with `WebhookDefinition$targetAction`.
const SOURCE_ACTION: &str = "Src";

/// A pipeline named `name` whose first stage is a `GitHub` source action, with the
/// non-`Source` stage every declaration has to carry
/// (`state::validate_declaration`).
fn pipeline(name: &str) -> Value {
    json!({
        "name": name,
        "roleArn": ROLE_ARN,
        "artifactStore": {"type": "S3", "location": "my-bucket"},
        "stages": [
            {
                "name": "Source",
                "actions": [{
                    "name": SOURCE_ACTION,
                    "actionTypeId": {
                        "category": "Source",
                        "owner": "ThirdParty",
                        "provider": "GitHub",
                        "version": "1",
                    },
                    "configuration": {"S3Bucket": "my-bucket", "S3ObjectKey": "app.zip"},
                    "outputArtifacts": [{"name": "SourceOut"}],
                }],
            },
            build_stage("Build", 1),
        ],
    })
}

/// A GITHUB_HMAC `WebhookDefinition` named `name` pointing at `target_pipeline` — the
/// authentication the third-party connection is made with, configured as
/// `WebhookAuthConfiguration` documents ("For GITHUB_HMAC, only the SecretToken property
/// must be set").
fn definition(name: &str, target_pipeline: &str) -> Value {
    json!({
        "name": name,
        "targetPipeline": target_pipeline,
        "targetAction": SOURCE_ACTION,
        "filters": [{"jsonPath": "$.ref", "matchEquals": "refs/heads/main"}],
        "authentication": AUTHENTICATION_GITHUB_HMAC,
        "authenticationConfiguration": {"SecretToken": SECRET_TOKEN},
    })
}

/// The names a row drives the rule with: a webhook name and the pipeline name its
/// definition points at, at both ends of the modelled 1..100 length and over the
/// character classes the shared pattern allows.
fn name_rows() -> Vec<(&'static str, String, String)> {
    vec![
        (
            "names of the modelled minimum length",
            "w".to_string(),
            "p".to_string(),
        ),
        (
            "names of the modelled maximum length",
            "w".repeat(100),
            "p".repeat(100),
        ),
        (
            "every character class the shared pattern allows",
            "Hook.name@9-_x".to_string(),
            "Pipe.line@9-_x".to_string(),
        ),
    ]
}

/// How the stored reference comes to name no pipeline the account holds.
#[derive(Clone, Copy, Debug)]
enum Absence {
    /// The webhook was defined over a pipeline name that was never created.
    NeverCreated,
    /// The pipeline existed when the webhook was defined and was deleted afterwards.
    DeletedAfterDefinition,
    /// The webhook was defined over a pipeline the account holds, then re-pointed by a
    /// later `PutWebhook` at a name it does not.
    RepointedByPutWebhook,
}

/// The pipeline a re-pointed webhook was defined over first.
const HELD_PIPELINE: &str = "held-pipeline";

/// Store a webhook named `name` whose target pipeline is absent in the way `absence`
/// describes, returning the emulator holding it.
async fn emulator_with_dangling_webhook(name: &str, target: &str, absence: Absence) -> Emulator {
    let emulator = Emulator::new();
    match absence {
        Absence::NeverCreated => {
            emulator
                .ok("PutWebhook", &json!({"webhook": definition(name, target)}))
                .await;
        }
        Absence::DeletedAfterDefinition => {
            emulator
                .ok("CreatePipeline", &json!({"pipeline": pipeline(target)}))
                .await;
            emulator
                .ok("PutWebhook", &json!({"webhook": definition(name, target)}))
                .await;
            emulator
                .ok("DeletePipeline", &json!({"name": target}))
                .await;
        }
        Absence::RepointedByPutWebhook => {
            emulator
                .ok("CreatePipeline", &json!({"pipeline": pipeline(HELD_PIPELINE)}))
                .await;
            emulator
                .ok(
                    "PutWebhook",
                    &json!({"webhook": definition(name, HELD_PIPELINE)}),
                )
                .await;
            // While the reference is held, the connection is served — so the refusal
            // below is the re-pointing and not the webhook.
            let (status, answer) = emulator
                .call(DEREGISTER, &json!({"webhookName": name}))
                .await;
            assert_eq!(
                status, 200,
                "{absence:?}: a webhook whose target pipeline is held is served before it \
                 is re-pointed, got {status} with {answer}"
            );
            emulator
                .ok("PutWebhook", &json!({"webhook": definition(name, target)}))
                .await;
        }
    }
    emulator
}

/// The names `ListWebhooks` reports, paired with the target pipeline each stored
/// definition carries.
async fn listed_webhooks(emulator: &Emulator) -> Vec<(String, String)> {
    let listed = emulator.ok("ListWebhooks", &json!({})).await;
    listed["webhooks"]
        .as_array()
        .unwrap_or_else(|| panic!("ListWebhooks reports a webhooks list, got {listed}"))
        .iter()
        .map(|item| {
            (
                item["definition"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                item["definition"]["targetPipeline"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// RULE: a third-party webhook operation resolves the stored
/// `WebhookDefinition$targetPipeline` against the account's pipelines on every call and
/// refuses with `ValidationException` naming the webhook and that pipeline when the
/// account holds none of that name, rather than answering its empty success.
#[tokio::test]
async fn a_dangling_target_pipeline_is_refused_not_reported_as_deregistered() {
    for (names_case, name, target) in name_rows() {
        for absence in [
            Absence::NeverCreated,
            Absence::DeletedAfterDefinition,
            Absence::RepointedByPutWebhook,
        ] {
            let case = format!("{names_case} with {absence:?}");
            let emulator = emulator_with_dangling_webhook(&name, &target, absence).await;

            // The dangling reference is what the store holds, and the listing shows it:
            // defining a webhook does not resolve the reference.
            let listed = listed_webhooks(&emulator).await;
            assert!(
                listed.contains(&(name.clone(), target.clone())),
                "case {case:?}: the stored definition points at {target:?}, got {listed:?}"
            );

            for operation in [REGISTER, DEREGISTER] {
                let (status, code, message) = emulator
                    .refusal(operation, &json!({"webhookName": &name}))
                    .await;
                // The sentence is written out here, so a reword of the handler's text
                // fails this suite rather than following it: the fixed text is AWS's and
                // the two variable parts come from the request and from state.
                let expected =
                    format!("Webhook {name} references a pipeline {target} which does not exist");
                assert_eq!(
                    (status, code.as_str(), message.as_str()),
                    (REFUSAL_STATUS, VALIDATION, expected.as_str()),
                    "case {case:?} through {operation}: expected {REFUSAL_STATUS} \
                     {VALIDATION} {expected:?}, got {status} {code} {message:?}"
                );
                assert_eq!(
                    webhook_target_pipeline_absent_message(&name, &target),
                    expected,
                    "case {case:?}: both operations state the one rule one way"
                );
            }

            // The refusal changed nothing: the webhook is still stored under its name
            // with the same dangling reference.
            let listed = listed_webhooks(&emulator).await;
            assert!(
                listed.contains(&(name.clone(), target.clone())),
                "case {case:?}: a refused call left the webhook as it was, got {listed:?}"
            );

            // CONTROL: once the account holds a pipeline of that name, the same calls
            // are served with the empty success — the refusal reports the store, not the
            // request.
            emulator
                .ok("CreatePipeline", &json!({"pipeline": pipeline(&target)}))
                .await;
            for operation in [REGISTER, DEREGISTER] {
                let (status, answer) = emulator
                    .call(operation, &json!({"webhookName": &name}))
                    .await;
                assert_eq!(
                    (status, answer.get("__type")),
                    (200, None),
                    "case {case:?} through {operation}: a held target pipeline is served \
                     with the empty success, got {status} with {answer}"
                );
            }
        }
    }
}

/// RULE, on the path a caller takes: the refusal is reachable after the webhook has been
/// defined and listed, is repeatable (resolving the reference is a read), and gives way
/// to the empty success exactly when a pipeline of that name is held — while a name no
/// webhook is stored under is still the `WebhookNotFoundException` the model declares.
#[tokio::test]
async fn the_dangling_target_pipeline_refusal_is_reachable_and_repeatable() {
    let emulator = Emulator::new();
    let name = "cp-webhook";
    let target = "cp-pipeline-ref";

    emulator
        .ok("PutWebhook", &json!({"webhook": definition(name, target)}))
        .await;
    let listed = listed_webhooks(&emulator).await;
    assert_eq!(
        listed,
        vec![(name.to_string(), target.to_string())],
        "the webhook is listed with its dangling target pipeline, got {listed:?}"
    );

    let expected = format!("Webhook {name} references a pipeline {target} which does not exist");
    // Twice: the refusal is computed from state each time, and answering it consumed
    // nothing.
    for attempt in 1..=2 {
        let (status, code, message) = emulator
            .refusal(DEREGISTER, &json!({"webhookName": name}))
            .await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (REFUSAL_STATUS, VALIDATION, expected.as_str()),
            "attempt {attempt}: expected {REFUSAL_STATUS} {VALIDATION} {expected:?}, got \
             {status} {code} {message:?}"
        );
    }

    emulator
        .ok("CreatePipeline", &json!({"pipeline": pipeline(target)}))
        .await;
    let (status, answer) = emulator
        .call(DEREGISTER, &json!({"webhookName": name}))
        .await;
    assert_eq!(
        (status, answer.get("__type")),
        (200, None),
        "a held target pipeline is served with the empty success, got {status} with {answer}"
    );

    // Deregistering is not deleting: the webhook is still there, and once it IS deleted
    // the name resolves to nothing and the model's not-found error is what the operation
    // answers — a condition distinct from the dangling reference above.
    let listed = listed_webhooks(&emulator).await;
    assert_eq!(
        listed,
        vec![(name.to_string(), target.to_string())],
        "deregistering left the webhook where it is, got {listed:?}"
    );
    emulator.ok("DeleteWebhook", &json!({"name": name})).await;
    let (status, code, _) = emulator
        .refusal(DEREGISTER, &json!({"webhookName": name}))
        .await;
    assert_eq!(
        (status, code.as_str()),
        (REFUSAL_STATUS, NOT_FOUND),
        "a name no stored webhook carries is the not-found refusal, got {status} {code}"
    );
}
