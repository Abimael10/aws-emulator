// SPDX-License-Identifier: CC0-1.0

//! Cover for the sentence `CreateWebhook` states when the project it names already carries
//! a webhook, driven over the wire the router serves: `POST /` with
//! `X-Amz-Target: CodeBuild_20161006.CreateWebhook` on awsJson1_1.
//!
//! A project holds at most ONE webhook, so a second creation is the duplicate-resource
//! refusal `ResourceAlreadyExistsException` models — and the sentence it carries is FIXED:
//! it names the webhook and `this project`, never the project the request sent nor its
//! ARN, so a client keying on that branch sees one sentence for every project and every
//! configuration. The table drives the whole family of projects that take a webhook (the
//! model's `ServerType` set, plus the runner webhook a `NO_SOURCE` project takes) crossed
//! with duplicate requests that repeat the held webhook's configuration and that differ
//! from it, and the controls prove the held webhook survived the refusal and that the same
//! call succeeds once the webhook is gone.

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::state;
use smithy_server_runtime::MultiServiceRouter;

use super::project_build_lifecycle::{
    call, create_project, emulator, error_type, merged, source_of,
};

/// Every project a webhook can be created for, as (source type, the `CreateWebhook`
/// members such a project's webhook needs).
///
/// The source types come from `state::WEBHOOK_SOURCE_TYPES`, which is the model's
/// `ServerType` set (`smithy_query(query="shape", shape="ServerType")`); the last row is
/// the runner webhook AWS documents on `CreateWebhookInput$buildType` as "only available
/// for NO_SOURCE source type projects", so it is the one webhook a project with no
/// repository provider takes. `manualCreation` is a GitHub-only member, so the second
/// GitHub row is the creation CodeBuild performs no provider-side call for.
fn webhook_capable_projects() -> Vec<(&'static str, Value)> {
    vec![
        ("BITBUCKET", json!({})),
        ("GITHUB", json!({})),
        ("GITHUB", json!({"manualCreation": true})),
        ("GITHUB_ENTERPRISE", json!({})),
        ("GITLAB", json!({})),
        ("GITLAB_SELF_MANAGED", json!({})),
        ("NO_SOURCE", json!({"buildType": state::RUNNER_WEBHOOK_BUILD_TYPE})),
    ]
}

/// Hold an access token for `server_type` when its provider needs one, so a row below is
/// decided by the duplicate-webhook rule and not by a missing credential (the rule that
/// owns tokens is `tests/all/webhook_access_token.rs`).
async fn import_source_credentials(router: &MultiServiceRouter, server_type: &str) {
    if !state::WEBHOOK_SOURCE_TYPES.contains(&server_type) {
        return;
    }
    let (status, body) = call(
        router,
        "ImportSourceCredentials",
        json!({
            "serverType": server_type,
            "authType": "PERSONAL_ACCESS_TOKEN",
            "token": "an-access-token",
            "shouldOverwrite": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ImportSourceCredentials ({server_type}): expected 200, got {status} with {body}"
    );
}

/// The webhook a read of `project` reports, through the implemented reader
/// `BatchGetProjects`.
async fn read_webhook(router: &MultiServiceRouter, project: &str) -> Value {
    let (status, body) = call(router, "BatchGetProjects", json!({"names": [project]})).await;
    assert_eq!(
        status, 200,
        "BatchGetProjects {project:?}: expected 200, got {status} with {body}"
    );
    body["projects"][0]["webhook"].clone()
}

/// RULE: creating a webhook for a project that already carries one is refused with HTTP
/// 400 `ResourceAlreadyExistsException` stating `Webhook already exists for this project` —
/// wholly fixed text that interpolates neither the project name nor its ARN, whatever the
/// project's source is and whether or not the duplicate request repeats the held webhook's
/// configuration.
#[tokio::test]
async fn a_second_webhook_for_one_project_states_codebuilds_fixed_duplicate_sentence() {
    let router = emulator();

    for (index, (source_type, webhook_members)) in webhook_capable_projects().iter().enumerate() {
        let project = format!("dup-webhook-{index}-{source_type}");
        create_project(&router, &project, json!({"source": source_of(source_type)})).await;
        import_source_credentials(&router, source_type).await;

        let created = {
            let request = merged(
                json!({"projectName": project.clone()}),
                webhook_members.clone(),
            );
            let (status, body) = call(&router, "CreateWebhook", request).await;
            assert_eq!(
                status, 200,
                "CreateWebhook {project:?}: expected 200, got {status} with {body}"
            );
            body["webhook"].clone()
        };

        // The duplicate requests: the same configuration again, and two that differ from
        // the held webhook — so the sentence is proved independent of what was asked for.
        let duplicates = [
            json!({}),
            json!({"branchFilter": "^refs/heads/main$"}),
            json!({"filterGroups": [[{"type": "EVENT", "pattern": "PUSH"}]]}),
        ];
        for duplicate in duplicates {
            let request = merged(
                merged(
                    json!({"projectName": project.clone()}),
                    webhook_members.clone(),
                ),
                duplicate.clone(),
            );
            let case = format!("CreateWebhook {project:?} again with {duplicate}");
            let (status, body) = call(&router, "CreateWebhook", request).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected 400, got {status} with {body}"
            );
            assert_eq!(
                error_type(&body),
                "ResourceAlreadyExistsException",
                "case {case:?}: expected the duplicate-resource error, got {body}"
            );
            let stated = body["message"].as_str().unwrap_or_default();
            assert_eq!(
                stated,
                state::WEBHOOK_ALREADY_EXISTS_MESSAGE,
                "case {case:?}: expected CodeBuild's fixed duplicate-webhook sentence, got \
                 {stated:?}"
            );
            assert!(
                !stated.contains("arn:") && !stated.contains(project.as_str()),
                "case {case:?}: the sentence must name neither the project nor its ARN, got \
                 {stated:?}"
            );

            // The refusal wrote nothing: the webhook the first call created is still the
            // one the project carries, configuration included.
            let held = read_webhook(&router, &project).await;
            assert_eq!(
                held["payloadUrl"], created["payloadUrl"],
                "case {case:?}: the held webhook must survive the refusal, got {held}"
            );
            assert_eq!(
                held["branchFilter"], created["branchFilter"],
                "case {case:?}: the refused request must not rewrite the held webhook, got \
                 {held}"
            );
        }

        // The control: with the webhook gone, the very same call succeeds — so every
        // refusal above was decided by the webhook already being there.
        let (status, body) = call(
            &router,
            "DeleteWebhook",
            json!({"projectName": project.clone()}),
        )
        .await;
        assert_eq!(
            status, 200,
            "DeleteWebhook {project:?}: expected 200, got {status} with {body}"
        );
        let request = merged(
            json!({"projectName": project.clone()}),
            webhook_members.clone(),
        );
        let (status, body) = call(&router, "CreateWebhook", request).await;
        assert_eq!(
            status, 200,
            "CreateWebhook {project:?} after a delete: expected 200, got {status} with {body}"
        );
    }
}
