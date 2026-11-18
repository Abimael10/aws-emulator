// SPDX-License-Identifier: CC0-1.0

//! The sentence each CodeBuild project refusal states, driven across the two services one
//! half of it spans.
//!
//! RULE: an HTTP status, an error code and an error MESSAGE are all part of a refusal's
//! contract, and CodeBuild words the message per error class: its fixed text stands as the
//! service writes it, and the only variable part is what that operation itself interpolates
//! — the identifier the request sent, or the ARN the request resolves to in the region it is
//! signed for. So the refusals below are asserted on the whole sentence: the role a request
//! hands CodeBuild to run as is refused with the three numbered conditions AWS states and no
//! part of the request quoted back, while the four operations that address ONE project each
//! state their OWN absence sentence rather than one shared template.
//!
//! The role half of that rule spans two services — CodeBuild resolves the role against the
//! ACCOUNT's IAM records — which is why this suite lives here and creates every role through
//! IAM's own `CreateRole` on the engine that serves CodeBuild, so what the check reads is the
//! document IAM persisted.
//!
//! The rows differ along what the rule generalises over: WHY the role cannot be assumed
//! (this account holds no such role, its trust policy names another service, it denies this
//! one, it names this one for another action), WHICH operation resolves the role (a
//! project's own `serviceRole` and one build's `serviceRoleOverride`), WHICH operation
//! addresses the absent project, and the form of the project name (the shortest the modelled
//! pattern allows, each permitted punctuation, a long one, a held project's name in another
//! case).

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder that builds the form body of an awsQuery call and the reader that
// pulls one element's text out of the XML answer are the test crate's own, written once in
// `all/query_wire.rs`.
use crate::query_wire::{element, form_encode};

/// The region every CodeBuild request below is signed for — deliberately not the emulator's
/// default, so an ARN a refusal interpolates proves the region came from the request.
const REGION: &str = "us-west-2";

/// The sentence CodeBuild states when it cannot assume the service role a request hands it,
/// word for word as the service answers it: fixed text naming the pass it could not make and
/// then the THREE conditions the caller has to check, with nothing of the request in it.
const UNASSUMABLE_SERVICE_ROLE: &str = "CodeBuild is not authorized to perform: \
                                        sts:AssumeRole on service role. Please verify that: \
                                        1) The provided service role exists, 2) The role \
                                        name is case-sensitive and matches exactly, and 3) \
                                        The role has the necessary trust policy configured.";

/// What the operation that empties ONE project's build cache states for a project this
/// account does not hold: the absence sentence with the name the request sent quoted after
/// it, terminated.
fn cache_reset_absence(project: &str) -> String {
    format!("Project does not exist: {project}.")
}

/// What the operation that changes a project's build visibility states for the same absence:
/// the sentence alone, terminated, with neither the ARN it was addressed by nor the name
/// inside it.
const VISIBILITY_ABSENCE: &str = "Project does not exist.";

/// What an operation that MANAGES a project's configuration states for the same absence: the
/// sentence unterminated, naming nothing of the request.
const MANAGEMENT_ABSENCE: &str = "Project does not exist";

/// What the operation that STARTS a build states for the same absence: its own wording, with
/// the ARN the requested name resolves to in the region the call is signed for.
fn start_absence(project: &str) -> String {
    format!("Project cannot be found: {}", project_arn(project))
}

/// Project names a request can address, spanning the forms the modelled `ProjectName` takes:
/// the shortest it allows, each permitted punctuation, a long one, and a held project's name
/// in another case.
///
/// From `smithy_query(query="shape",shape="ProjectName")`:
/// `^[A-Za-z0-9][A-Za-z0-9\-_]{1,149}$`, length 2-150.
const ABSENT_PROJECTS: [&str; 5] = [
    "ab",
    "absent-project",
    "absent_project",
    "a0123456789012345678901234567890123456789012345678901234567890123456789",
    "HELD-PROJECT",
];

/// The project this account DOES hold, so every refusal above is decided by the name asked
/// about and not by the account holding nothing at all.
const HELD_PROJECT: &str = "held-project";

/// One emulator carrying both services over ONE shared engine, exactly as the production
/// router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(engine),
    );
    router
}

/// One awsQuery call to IAM, answered with the HTTP status and the XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, body)
}

/// One awsJson1_1 call to CodeBuild, answered with the HTTP status and the JSON body.
async fn codebuild(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("codebuild.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CodeBuild_20161006.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/codebuild/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=abc"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the test body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Call `operation`, assert it was refused with HTTP 400 and `code`, and hand back the
/// sentence it stated.
async fn refusal(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
    code: &str,
) -> String {
    let (status, answer) = codebuild(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400 from {operation}, got {status} with {answer}"
    );
    assert_eq!(
        answer["__type"].as_str().unwrap_or_default(),
        code,
        "case {case:?}: expected {code} from {operation}, got {answer}"
    );
    answer["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {operation} must state a message, got {answer}"))
        .to_string()
}

/// A trust policy naming `principals` in one `Allow` of the assume.
fn trust_allowing(principals: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principals },
            "Action": "sts:AssumeRole"
        }]
    })
    .to_string()
}

/// Create a role in IAM carrying `document` and hand back the ARN IAM holds it under.
async fn role_with_trust(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", document)],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateRole {name:?}: expected the role to be persisted, got {status}: {created}"
    );
    element(&created, "Arn")
}

/// A `CreateProject` request for `name` that runs as `service_role`.
fn create_project_body(name: &str, service_role: &str) -> Value {
    json!({
        "name": name,
        "source": {"type": "NO_SOURCE", "buildspec": "version: 0.2"},
        "artifacts": {"type": "NO_ARTIFACTS"},
        "environment": {
            "type": "LINUX_CONTAINER",
            "image": "aws/codebuild/standard:7.0",
            "computeType": "BUILD_GENERAL1_SMALL",
        },
        "serviceRole": service_role,
    })
}

/// The ARN a project `name` of this account is addressed by in the signed region.
fn project_arn(name: &str) -> String {
    format!(
        "arn:aws:codebuild:{REGION}:{}:project/{name}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// RULE: a request that hands CodeBuild a service role it cannot assume is refused with 400
/// `InvalidInputException` stating AWS's own three numbered conditions — the SAME fixed
/// sentence whatever made the role unassumable, with no part of the request quoted back, and
/// the same one wherever a role is resolved.
///
/// The rows cross every reason a role cannot be assumed with both operations that resolve
/// one, and each refusal is checked to have left no project and no build behind.
#[tokio::test]
async fn an_unassumable_service_role_states_awss_own_three_conditions() {
    let router = emulator();

    // The project a build's override is refused against, which runs as a role this service
    // may assume.
    let own_role = role_with_trust(
        &router,
        "codebuild-wording-own",
        &trust_allowing(&["codebuild.amazonaws.com"]),
    )
    .await;
    let (status, created) = codebuild(
        &router,
        "CreateProject",
        create_project_body("wording-proj", &own_role),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateProject for the suite's own project: expected 200, got {status} with {created}"
    );

    // A role whose trust policy names this service for an action that is not the assume:
    // a trust policy governs `sts:AssumeRole`, so a statement about anything else grants
    // nothing.
    let other_action = role_with_trust(
        &router,
        "codebuild-wording-action",
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": "codebuild.amazonaws.com" },
                "Action": "s3:GetObject"
            }]
        })
        .to_string(),
    )
    .await;
    let denied = role_with_trust(
        &router,
        "codebuild-wording-denied",
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Deny",
                "Principal": { "Service": "codebuild.amazonaws.com" },
                "Action": "sts:AssumeRole"
            }]
        })
        .to_string(),
    )
    .await;
    let other_service = role_with_trust(
        &router,
        "codebuild-wording-other",
        &trust_allowing(&["ec2.amazonaws.com"]),
    )
    .await;

    // (case, the role a request hands CodeBuild)
    let rows: [(&str, String); 4] = [
        (
            "a role this account's IAM holds nothing under",
            format!(
                "arn:aws:iam::{}:role/codebuild-wording-absent",
                engine::EMULATOR_ACCOUNT_ID
            ),
        ),
        (
            "a role trusting this service for an action that is not the assume",
            other_action,
        ),
        ("a role trusted by another service only", other_service),
        ("a role whose trust policy denies this service", denied),
    ];

    let mut stated = BTreeSet::new();
    for (index, (case, role_arn)) in rows.iter().enumerate() {
        let project = format!("wording-refused-{index}");

        // The role the PROJECT would run as.
        let sentence = refusal(
            &router,
            case,
            "CreateProject",
            create_project_body(&project, role_arn),
            "InvalidInputException",
        )
        .await;
        assert_eq!(
            sentence, UNASSUMABLE_SERVICE_ROLE,
            "case {case:?}: CreateProject must state AWS's three conditions, got {sentence:?}"
        );
        stated.insert(sentence);

        // The role ONE BUILD would run as: the same question, so the same sentence.
        let sentence = refusal(
            &router,
            case,
            "StartBuild",
            json!({"projectName": "wording-proj", "serviceRoleOverride": role_arn}),
            "InvalidInputException",
        )
        .await;
        assert_eq!(
            sentence, UNASSUMABLE_SERVICE_ROLE,
            "case {case:?}: StartBuild must state AWS's three conditions, got {sentence:?}"
        );
        assert!(
            !sentence.contains("arn:")
                && !sentence.contains(role_arn.as_str())
                && !sentence.contains(&project),
            "case {case:?}: the sentence quotes no part of the request, got {sentence:?}"
        );
        stated.insert(sentence);

        // A refused request wrote nothing: the project was never stored, and the project the
        // override was refused against started no build.
        let (status, read) = codebuild(&router, "BatchGetProjects", json!({"names": [&project]})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: BatchGetProjects: expected 200, got {status} with {read}"
        );
        assert_eq!(
            read["projects"].as_array().map(Vec::len).unwrap_or_default(),
            0,
            "case {case:?}: a refused create holds no project, got {read}"
        );
        let (status, listed) = codebuild(
            &router,
            "ListBuildsForProject",
            json!({"projectName": "wording-proj"}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListBuildsForProject: expected 200, got {status} with {listed}"
        );
        assert_eq!(
            listed["ids"].as_array().map(Vec::len).unwrap_or_default(),
            0,
            "case {case:?}: a refused override started no build, got {listed}"
        );
    }

    // One sentence for every reason and both operations: the refusal is fixed text, so the
    // set of sentences collected above holds exactly one member.
    assert_eq!(
        stated.len(),
        1,
        "the refusal must be one fixed sentence, got {stated:?}"
    );

    // The control: the role this account holds AND trusts this service is accepted, so every
    // row above was decided by the role and not by the operation.
    let (status, created) = codebuild(
        &router,
        "CreateProject",
        create_project_body("wording-accepted", &own_role),
    )
    .await;
    assert_eq!(
        status, 200,
        "a role codebuild may assume must be accepted, got {status} with {created}"
    );
    assert_eq!(
        created["project"]["serviceRole"].as_str(),
        Some(own_role.as_str()),
        "the project runs as the role it was created with, got {created}"
    );
}

/// RULE: each operation that addresses ONE project states, for a project this account does
/// not hold, the sentence CodeBuild words for THAT operation — the cache reset quotes the
/// name it was given and terminates it, the visibility change states the sentence alone
/// (quoting neither the ARN it was addressed by nor the name inside it), a management call
/// states it unterminated, and a build start quotes the ARN the name resolves to — so no one
/// template can serve them and a message-matching client sees the same bytes AWS answers
/// with.
///
/// The rows cross all four operations with every form of an absent project name, and the
/// visibility change is asked about two different absent ARNs to prove its sentence does not
/// vary with the project at all.
#[tokio::test]
async fn each_absent_project_refusal_states_only_what_that_operation_interpolates() {
    let router = emulator();
    let role = role_with_trust(
        &router,
        "codebuild-absence-role",
        &trust_allowing(&["codebuild.amazonaws.com"]),
    )
    .await;
    let (status, created) = codebuild(
        &router,
        "CreateProject",
        create_project_body(HELD_PROJECT, &role),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateProject {HELD_PROJECT:?}: expected 200, got {status} with {created}"
    );

    for project in ABSENT_PROJECTS {
        let case = format!("InvalidateProjectCache for {project:?}");
        let cache = refusal(
            &router,
            &case,
            "InvalidateProjectCache",
            json!({"projectName": project}),
            "ResourceNotFoundException",
        )
        .await;
        assert_eq!(
            cache,
            cache_reset_absence(project),
            "case {case:?}: expected the name the request sent, quoted and terminated, got \
             {cache:?}"
        );
        assert!(
            !cache.contains("arn:"),
            "case {case:?}: the name is quoted as sent, not as an ARN, got {cache:?}"
        );

        let case = format!("UpdateProjectVisibility for {project:?}");
        let visibility = refusal(
            &router,
            &case,
            "UpdateProjectVisibility",
            json!({"projectArn": project_arn(project), "projectVisibility": "PRIVATE"}),
            "ResourceNotFoundException",
        )
        .await;
        assert_eq!(
            visibility, VISIBILITY_ABSENCE,
            "case {case:?}: expected the terminated sentence alone, got {visibility:?}"
        );
        assert!(
            !visibility.contains("arn:") && !visibility.contains(project),
            "case {case:?}: the sentence names neither the project nor its ARN, got \
             {visibility:?}"
        );

        let case = format!("UpdateProject for {project:?}");
        let management = refusal(
            &router,
            &case,
            "UpdateProject",
            json!({"name": project}),
            "ResourceNotFoundException",
        )
        .await;
        assert_eq!(
            management, MANAGEMENT_ABSENCE,
            "case {case:?}: expected the unterminated sentence, got {management:?}"
        );

        let case = format!("StartBuild for {project:?}");
        let start = refusal(
            &router,
            &case,
            "StartBuild",
            json!({"projectName": project}),
            "ResourceNotFoundException",
        )
        .await;
        assert_eq!(
            start,
            start_absence(project),
            "case {case:?}: expected the ARN the name resolves to, got {start:?}"
        );

        // Four operations, four sentences for the SAME absent project: the wording is per
        // operation rather than one shared template.
        let sentences: BTreeSet<String> = [cache, visibility, management, start].into();
        assert_eq!(
            sentences.len(),
            4,
            "case {project:?}: each operation states its own sentence, got {sentences:?}"
        );
    }

    // The visibility change's sentence does not vary with the project it was asked about:
    // two different absent ARNs, including one that is not even a project ARN of this
    // account, state the same bytes.
    let addressed: [(&str, String); 2] = [
        ("another absent project's ARN", project_arn("second-absent")),
        (
            "an absent project's ARN in another region",
            format!(
                "arn:aws:codebuild:eu-west-1:{}:project/third-absent",
                engine::EMULATOR_ACCOUNT_ID
            ),
        ),
    ];
    for (case, arn) in &addressed {
        let stated = refusal(
            &router,
            case,
            "UpdateProjectVisibility",
            json!({"projectArn": arn, "projectVisibility": "PUBLIC_READ"}),
            "ResourceNotFoundException",
        )
        .await;
        assert_eq!(
            stated, VISIBILITY_ABSENCE,
            "case {case:?}: the sentence is the same for every absent project, got {stated:?}"
        );
    }

    // The controls: the project this account DOES hold is acted on rather than refused, so
    // every row above was decided by the name asked about.
    let (status, reset) = codebuild(
        &router,
        "InvalidateProjectCache",
        json!({"projectName": HELD_PROJECT}),
    )
    .await;
    assert_eq!(
        status, 200,
        "InvalidateProjectCache for the held project: expected 200, got {status} with {reset}"
    );
    let (status, changed) = codebuild(
        &router,
        "UpdateProjectVisibility",
        json!({"projectArn": project_arn(HELD_PROJECT), "projectVisibility": "PRIVATE"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "UpdateProjectVisibility for the held project: expected 200, got {status} with {changed}"
    );
    let (status, updated) = codebuild(
        &router,
        "UpdateProject",
        json!({"name": HELD_PROJECT, "description": "changed"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "UpdateProject for the held project: expected 200, got {status} with {updated}"
    );
}
