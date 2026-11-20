// SPDX-License-Identifier: CC0-1.0

//! Cover for the PARTIAL-UPDATE rule on `UpdateWebhook`, driven over the wire the router
//! serves: `POST /` with `X-Amz-Target: CodeBuild_20161006.<Operation>` on awsJson1_1.
//!
//! RULE under test: every member of `UpdateWebhookInput` beside the `projectName` that
//! identifies the webhook is OPTIONAL, so an update applies only the configuration the
//! request CARRIES — a configuration member the request omits keeps the value the stored
//! webhook already has, and an omitted member is no value for a precondition to judge
//! either, so the build-type/source-type pairing is decided only against a `buildType`
//! the request actually supplied. The shortest form of the call, `projectName` alone,
//! therefore changes no configuration at all and reports the webhook as it stands.
//!
//! `UpdateWebhookInput` models `projectName` (required) plus `branchFilter`,
//! `rotateSecret`, `filterGroups`, `buildType` (`WebhookBuildType`: `BUILD`,
//! `BUILD_BATCH`, `RUNNER_BUILDKITE_BUILD`) and `pullRequestBuildPolicy`, and answers
//! with `{webhook}`
//! (`smithy_query(query="operation",operation="CreateWebhook,UpdateWebhook")`,
//! `smithy_query(query="shape",shape="WebhookBuildType,WebhookFilterType")`).
//!
//! The rows span what the rule generalises over: every kind of webhook a project can
//! carry — each source type that sends repository events (`state::WEBHOOK_SOURCE_TYPES`)
//! and the runner webhook's own source type (`state::RUNNER_WEBHOOK_SOURCE_TYPE`),
//! crossed with the build types each takes and with a webhook created carrying no build
//! type at all — against every form of request that omits something: nothing but the
//! project name, a secret rotation, and each configuration member on its own.

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::state;
use smithy_server_runtime::MultiServiceRouter;

use super::project_build_lifecycle::{call, create_project_body, emulator, error_type, source_of};
// The member lists and the merge step the partial-update rule is judged with live in the
// suite that owns the webhook lifecycle, so both suites judge one configuration rule.
use super::webhook_lifecycle::{assert_same_configuration, configuration_after, IDENTITY_MEMBERS};

/// A repository-event `WebhookBuildType`, as opposed to the runner one
/// (`state::RUNNER_WEBHOOK_BUILD_TYPE`): the model declares `BUILD`, `BUILD_BATCH` and
/// `RUNNER_BUILDKITE_BUILD` (`smithy_query(query="shape", shape="WebhookBuildType")`).
const REPOSITORY_EVENT_BUILD_TYPES: &[&str] = &["BUILD", "BUILD_BATCH"];

/// Every kind of webhook a project can carry: the source type it belongs to, paired with
/// the `buildType` it was created with. A `NO_SOURCE` project's only webhook is the
/// runner one; every source type that sends repository events takes the repository-event
/// build types, and a create that names none at all leaves the member unset.
fn webhook_kinds() -> Vec<(&'static str, Option<&'static str>)> {
    let mut kinds: Vec<(&str, Option<&str>)> = vec![(
        state::RUNNER_WEBHOOK_SOURCE_TYPE,
        Some(state::RUNNER_WEBHOOK_BUILD_TYPE),
    )];
    for source_type in state::WEBHOOK_SOURCE_TYPES {
        kinds.push((source_type, Some(REPOSITORY_EVENT_BUILD_TYPES[0])));
    }
    kinds.push(("GITHUB", Some(REPOSITORY_EVENT_BUILD_TYPES[1])));
    kinds.push(("GITLAB", None));
    kinds
}

/// A build type the project's source DOES take, other than the one its webhook already
/// carries: the runner source type takes only the runner build type, and a source that
/// sends repository events takes the repository-event build type the webhook was not
/// created with.
fn replacement_build_type(source_type: &str, created: Option<&str>) -> &'static str {
    if source_type == state::RUNNER_WEBHOOK_SOURCE_TYPE {
        return state::RUNNER_WEBHOOK_BUILD_TYPE;
    }
    let other = REPOSITORY_EVENT_BUILD_TYPES
        .iter()
        .find(|build_type| Some(**build_type) != created);
    other.copied().unwrap_or(REPOSITORY_EVENT_BUILD_TYPES[0])
}

/// Request forms that OMIT at least one configuration member: the shortest call there
/// is, a secret rotation, and each configuration member carried on its own against the
/// webhook a row created (`source_type`, `created` build type).
///
/// The filter groups, branch filters and build type below differ from the ones the
/// webhook is created with, so a member that was wrongly cleared and a member that was
/// wrongly left alone are told apart. The filter types are the model's
/// (`smithy_query(query="shape", shape="WebhookFilterType")`).
fn omitting_requests(source_type: &str, created: Option<&str>) -> Vec<(&'static str, Value)> {
    vec![
        ("the project name alone", json!({})),
        ("a secret rotation only", json!({"rotateSecret": true})),
        (
            "a branch filter only",
            json!({"branchFilter": "^refs/heads/release/.*$"}),
        ),
        (
            "filter groups only",
            json!({"filterGroups": [[
                {"type": "EVENT", "pattern": "PULL_REQUEST_MERGED"},
                {"type": "ACTOR_ACCOUNT_ID", "pattern": "^1234$"},
            ]]}),
        ),
        (
            "a build type only",
            json!({"buildType": replacement_build_type(source_type, created)}),
        ),
        (
            "a pull request build policy only",
            json!({"pullRequestBuildPolicy": {"requiresCommentApproval": "DISABLED"}}),
        ),
    ]
}

/// One router whose account holds an access token for every provider CodeBuild
/// authenticates to, so no row below is decided by the access-token precondition (which
/// `tests/all/webhook_access_token.rs` owns).
async fn emulator_with_tokens() -> MultiServiceRouter {
    let router = emulator();
    for server_type in state::WEBHOOK_SOURCE_TYPES {
        let (status, body) = call(
            &router,
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
    router
}

/// The configuration a webhook is created with: every member an update can carry, so
/// each one can be seen to be kept or replaced. `nth` varies the filter groups by count,
/// filter type and ordering, and the pull-request policy is set only where a pull
/// request can reach the project at all.
fn created_configuration(build_type: Option<&str>, source_type: &str, nth: usize) -> Value {
    let groups = [
        json!([[{"type": "EVENT", "pattern": "PUSH"}]]),
        json!([[
            {"type": "EVENT", "pattern": "PULL_REQUEST_CREATED,PULL_REQUEST_UPDATED"},
            {"type": "HEAD_REF", "pattern": "^refs/heads/main$"},
        ]]),
        json!([
            [{"type": "EVENT", "pattern": "PUSH"}, {"type": "FILE_PATH", "pattern": "^src/"}],
            [{"type": "EVENT", "pattern": "PULL_REQUEST_REOPENED"}],
        ]),
    ];
    let mut configuration = json!({
        "branchFilter": format!("^refs/heads/main-{nth}$"),
        "filterGroups": groups[nth % groups.len()].clone(),
    });
    let members = configuration
        .as_object_mut()
        .expect("the configuration is an object");
    if let Some(build_type) = build_type {
        members.insert("buildType".to_string(), json!(build_type));
    }
    if state::WEBHOOK_SOURCE_TYPES.contains(&source_type) {
        members.insert(
            "pullRequestBuildPolicy".to_string(),
            json!({
                "requiresCommentApproval": "ALL_PULL_REQUESTS",
                "approverRoles": ["GITHUB_WRITE"],
            }),
        );
    }
    configuration
}

/// Create `project` with a primary source of `source_type` and connect `configuration` to
/// it as its webhook, handing back the webhook the creating call reported.
async fn connect(
    router: &MultiServiceRouter,
    project: &str,
    source_type: &str,
    configuration: &Value,
) -> Value {
    let (status, body) = call(
        router,
        "CreateProject",
        create_project_body(project, json!({"source": source_of(source_type)})),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateProject {project:?} ({source_type}): expected 200, got {status} with {body}"
    );

    let mut request = configuration.clone();
    request["projectName"] = json!(project);
    let (status, body) = call(router, "CreateWebhook", request).await;
    assert_eq!(
        status, 200,
        "CreateWebhook {project:?} ({source_type}): expected 200, got {status} with {body}"
    );
    body["webhook"].clone()
}

/// Call `UpdateWebhook` for `project` with `request` as the rest of the input.
async fn update(router: &MultiServiceRouter, project: &str, request: &Value) -> (u16, Value) {
    let mut body = request.clone();
    body["projectName"] = json!(project);
    call(router, "UpdateWebhook", body).await
}

/// The webhook a later read reports for `project`, through the implemented reader
/// `BatchGetProjects`.
async fn read_webhook(router: &MultiServiceRouter, case: &str, project: &str) -> Value {
    let (status, body) = call(router, "BatchGetProjects", json!({"names": [project]})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: BatchGetProjects must be served, got {status} with {body}"
    );
    body["projects"][0]["webhook"].clone()
}

/// Assert `actual` reports the same value as `expected` for each of `members`.
fn assert_members(case: &str, members: &[&str], actual: &Value, expected: &Value) {
    for member in members {
        assert_eq!(
            actual[*member], expected[*member],
            "case {case:?}: expected {member} {}, got {}",
            expected[*member], actual[*member]
        );
    }
}

/// THE RULE: whatever kind of webhook a project carries, an update applies the
/// configuration the request carries and keeps every member it omits — and the members
/// identifying the created webhook are left alone either way.
#[tokio::test]
async fn an_update_keeps_every_configuration_member_it_omits() {
    for (nth, (source_type, build_type)) in webhook_kinds().iter().enumerate() {
        for (form, request) in omitting_requests(source_type, *build_type) {
            let case = format!(
                "UpdateWebhook with {form} on a {source_type} webhook created with \
                 buildType {build_type:?}"
            );
            // A fresh emulator per row: no row's answer may depend on the one before.
            let router = emulator_with_tokens().await;
            let project = format!("uwp-{nth}-{}", form.replace(' ', "-"));
            let configuration = created_configuration(*build_type, source_type, nth);
            let created = connect(&router, &project, source_type, &configuration).await;
            let expected = configuration_after(&created, &request);

            let (status, body) = update(&router, &project, &request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} with {body}"
            );
            let updated = body["webhook"].clone();
            assert_same_configuration(&case, &updated, &expected);
            assert_members(&case, IDENTITY_MEMBERS, &updated, &created);

            // Round trip: the project reports exactly what the update left stored, and a
            // second read reports the same again.
            let stored = read_webhook(&router, &case, &project).await;
            assert_same_configuration(&format!("{case}: read back"), &stored, &expected);
            let again = read_webhook(&router, &case, &project).await;
            assert_eq!(
                again, stored,
                "case {case:?}: a second read must report the same webhook, got {again}"
            );
        }
    }
}

/// THE OTHER HALF: the build-type/source-type pairing is judged against a build type the
/// REQUEST carries, so an update that names one the project's source does not take is
/// still refused in CodeBuild's own words, while an update that names none is served and
/// the stored build type stands.
#[tokio::test]
async fn the_build_type_rule_judges_only_a_build_type_the_request_carries() {
    for (nth, (source_type, build_type)) in webhook_kinds().iter().enumerate() {
        let router = emulator_with_tokens().await;
        let project = format!("uwb-{nth}");
        let created = connect(
            &router,
            &project,
            source_type,
            &created_configuration(*build_type, source_type, nth),
        )
        .await;

        // The build type the project's source does NOT take: the runner one for a source
        // that sends repository events, a repository-event one for the runner's source.
        let refused = if *source_type == state::RUNNER_WEBHOOK_SOURCE_TYPE {
            REPOSITORY_EVENT_BUILD_TYPES[0]
        } else {
            state::RUNNER_WEBHOOK_BUILD_TYPE
        };
        let case = format!("UpdateWebhook to buildType {refused} on {source_type}");
        let (status, body) = update(&router, &project, &json!({"buildType": refused})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_type(&body),
            "InvalidInputException",
            "case {case:?}: expected InvalidInputException, got {body}"
        );
        assert_eq!(
            body["message"],
            json!(format!(
                "Invalid build type provided for {source_type} type webhook"
            )),
            "case {case:?}: expected CodeBuild's build-type sentence, got {body}"
        );
        assert_same_configuration(
            &format!("{case}: after the refusal"),
            &read_webhook(&router, &case, &project).await,
            &created,
        );

        // The same call with the member left OUT carries no build type to judge at all,
        // so it is served and the stored one stands — including on the source type whose
        // only webhook is the runner one.
        let case = format!("UpdateWebhook carrying no buildType on {source_type}");
        let (status, body) = update(&router, &project, &json!({"rotateSecret": true})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["webhook"]["buildType"], created["buildType"],
            "case {case:?}: expected the stored buildType {}, got {}",
            created["buildType"], body["webhook"]["buildType"]
        );
        assert_eq!(
            read_webhook(&router, &case, &project).await["buildType"],
            created["buildType"],
            "case {case:?}: the read must report the stored buildType {}",
            created["buildType"]
        );
    }
}
