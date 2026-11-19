// SPDX-License-Identifier: CC0-1.0

//! Who a `CodeBuild` Build action is performed AS, end to end over the wire this service
//! speaks (`awsJson1_1`, `POST /` with `X-Amz-Target: CodePipeline_20150709.<Operation>`).
//!
//! RULE under test: an action is performed under the role it names —
//! `ActionExecutionInput$roleArn` is the action's own `roleArn` or, where it declares none,
//! "the roleArn for the pipeline" — so the `StartBuild` call a `CodeBuild` action makes is
//! authorized against the permissions in force on THAT role, not against the caller's. A
//! role whose identity-based policies do not allow `codebuild:StartBuild` on the project the
//! action names cannot make the call, so NO build is created: the action fails with
//! `JobFailed` and reports no external execution, and the execution reaches `Failed`. A role
//! those policies do allow it for starts a real build, which the action then follows. A role
//! IAM holds no identity policy for is no evidence about what it may do — an execution role
//! is routinely provisioned out of band — so such a role is not held back.
//!
//! The decision is IAM's, over the documents IAM holds, so IAM is mounted on the same engine
//! as this service (as `core/router` assembles them) and the role is created through IAM's
//! own wire path (`CreateRole`, `PutRolePolicy`): the documents the decision reads are the
//! ones a client would have written. Amazon S3 and CodeBuild are mounted beside them because
//! the pipeline's source action reads an object and its build action starts a build.
//!
//! The rows differ along what the rule generalises over: whether the role's policies allow
//! the action at all, which RESOURCE they allow it on, whether an explicit `Deny` stands
//! beside an `Allow`, whether the grant is written with a wildcard, and whether the role
//! states any policy at all.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The `X-Amz-Target` prefix OPERATIONS.md records for this service.
const TARGET_PREFIX: &str = "CodePipeline_20150709";

/// The `X-Amz-Target` prefix CodeBuild's OPERATIONS.md records.
const CODEBUILD_TARGET: &str = "CodeBuild_20161006";

/// The region every request of this suite is signed for, and so the region of the project
/// ARN a decision is taken about.
const REGION: &str = "us-east-1";

/// The access key id every request presents: the account's own published credential, which
/// no identity-based policy constrains — the decision under test is about the PIPELINE's
/// role, not about the caller.
const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// The trust policy a pipeline's service role carries.
const PIPELINE_TRUST: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"codepipeline.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The trust policy a build project's service role carries.
const CODEBUILD_TRUST: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"codebuild.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The bucket the pipelines of this suite keep their artifacts in, and read their source
/// object from.
const BUCKET: &str = "build-authorization-artifacts";

/// The `FailureType` an action carries when the work it was to perform failed.
const JOB_FAILED: &str = "JobFailed";

/// The `PipelineExecutionStatus` values an execution never leaves.
const TERMINAL: [&str; 5] = [
    "Succeeded",
    "Failed",
    "Stopped",
    "Superseded",
    "Cancelled",
];

/// How long a wait for an execution to settle may take before the suite calls it stalled.
/// Generous on purpose: the wait ends on the CONDITION, and the deadline is only there so an
/// execution that never settles fails the test instead of hanging it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the wait below leaves before it looks again.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// One emulator: this service, IAM, Amazon S3 and CodeBuild over ONE shared engine, the way
/// the router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_codepipeline_codepipeline_20150709::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `signing_name`, in [`REGION`].
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host;x-amz-date, Signature=00"
    )
}

/// One awsJson1_1 call to the service `signing_name` claims, under `target`.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("x-amz-target", format!("{target}.{operation}"))
        .header("content-type", "application/x-amz-json-1.1")
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the harness builds a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CodePipeline operation that has to succeed.
async fn cp_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = json_call(router, "codepipeline", TARGET_PREFIX, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: codepipeline {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One CodeBuild operation that has to succeed.
async fn cb_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = json_call(router, "codebuild", CODEBUILD_TARGET, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: codebuild {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One awsQuery call against the IAM service mounted beside this one.
async fn iam_call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&crate::pipeline_lifecycle::form_encode(name));
        body.push('=');
        body.push_str(&crate::pipeline_lifecycle::form_encode(value));
    }
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("the harness builds a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8(response.into_body().to_vec()).expect("a UTF-8 body");
    (status, payload)
}

/// Create a role carrying `trust`, with `document` as its inline permissions policy when one
/// is given, and answer the ARN IAM holds it under.
async fn role_with(
    router: &MultiServiceRouter,
    name: &str,
    trust: &str,
    document: Option<&str>,
) -> String {
    let (status, body) = iam_call(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", trust)],
    )
    .await;
    assert_eq!(status, 200, "CreateRole {name} must succeed: {body}");
    if let Some(document) = document {
        let (status, body) = iam_call(
            router,
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "pipeline-permissions"),
                ("PolicyDocument", document),
            ],
        )
        .await;
        assert_eq!(status, 200, "PutRolePolicy {name} must succeed: {body}");
    }
    format!(
        "arn:aws:iam::{}:role/{name}",
        ::engine::emulator_account_id()
    )
}

/// One S3 request, path style, the way the emulated S3 service routes it.
async fn s3(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> u16 {
    let request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://s3.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("the harness builds a well-formed request");
    router.dispatch(request).await.status().as_u16()
}

/// The ARN of the project `name`, which is the resource the action's grant is about.
fn project_arn(name: &str) -> String {
    format!(
        "arn:aws:codebuild:{REGION}:{}:project/{name}",
        ::engine::emulator_account_id()
    )
}

/// An identity policy allowing `action` on `resource`, with `denied` added as an explicit
/// `Deny` of the same action when the row states one.
fn policy(action: &str, resource: &str, denied: Option<&str>) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": action,
        "Resource": resource,
    })];
    if let Some(denied) = denied {
        statements.push(json!({
            "Effect": "Deny",
            "Action": denied,
            "Resource": "*",
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// A pipeline reading one object and then building `project`, under `role_arn`.
fn declaration(name: &str, role_arn: &str, project: &str) -> Value {
    json!({"pipeline": {
        "name": name,
        "roleArn": role_arn,
        "artifactStore": {"type": "S3", "location": BUCKET},
        "stages": [
            {
                "name": "Source",
                "actions": [{
                    "name": "Src",
                    "actionTypeId": {
                        "category": "Source", "owner": "AWS", "provider": "S3", "version": "1",
                    },
                    "configuration": {
                        "S3Bucket": BUCKET,
                        "S3ObjectKey": "source.zip",
                        "PollForSourceChanges": "false",
                    },
                    "outputArtifacts": [{"name": "Src"}],
                }],
            },
            {
                "name": "Build",
                "actions": [{
                    "name": "Build",
                    "actionTypeId": {
                        "category": "Build", "owner": "AWS", "provider": "CodeBuild",
                        "version": "1",
                    },
                    "configuration": {"ProjectName": project},
                    "inputArtifacts": [{"name": "Src"}],
                }],
            },
        ],
    }})
}

/// Wait until every execution of `pipeline` has reached a terminal status, and answer the
/// newest one's status.
async fn settled_execution(router: &MultiServiceRouter, case: &str, pipeline: &str) -> String {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let listed = cp_ok(
            router,
            case,
            "ListPipelineExecutions",
            &json!({"pipelineName": pipeline}),
        )
        .await;
        let summaries = listed["pipelineExecutionSummaries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let settled = !summaries.is_empty()
            && summaries
                .iter()
                .all(|summary| TERMINAL.contains(&summary["status"].as_str().unwrap_or_default()));
        if settled {
            return summaries[0]["status"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: every execution of {pipeline} must settle, last read {listed}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The state of the Build action of `pipeline`, as `GetPipelineState` reports it.
async fn build_action_state(router: &MultiServiceRouter, case: &str, pipeline: &str) -> Value {
    let state = cp_ok(router, case, "GetPipelineState", &json!({"name": pipeline})).await;
    state["stageStates"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|stage| stage["stageName"].as_str() == Some("Build"))
        .and_then(|stage| {
            stage["actionStates"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .find(|action| action["actionName"].as_str() == Some("Build"))
        })
        .unwrap_or_else(|| panic!("case {case:?}: no state for Build/Build of {pipeline}"))
}

/// RULE: the Build action's `StartBuild` is authorized against the role the action is
/// performed under, so a role that may not start the build starts none.
#[tokio::test]
async fn a_build_action_starts_no_build_its_role_may_not_start() {
    let router = emulator();
    assert_eq!(
        s3(&router, "PUT", &format!("/{BUCKET}"), b"").await,
        200,
        "the artifact bucket must be created"
    );
    assert_eq!(
        s3(
            &router,
            "PUT",
            &format!("/{BUCKET}/source.zip"),
            // The object a source action delivers to a build is the ARCHIVE the build
            // unpacks into its working tree, so the fixture is one.
            &crate::execution_engine::zip_of(&[("app.txt", b"source")]),
        )
        .await,
        200,
        "the source object must be put"
    );
    let build_role = role_with(&router, "ba-codebuild-role", CODEBUILD_TRUST, None).await;

    /// One row: the case, the inline permissions policy the pipeline's role carries (or
    /// none), and whether the action may start its build.
    type Row = (&'static str, Option<String>, bool);

    let rows: Vec<Row> = vec![
        (
            "a role granted the action on the project",
            Some(policy(
                "codebuild:StartBuild",
                &project_arn("ba-project-0"),
                None,
            )),
            true,
        ),
        (
            "a role granted every action of the service on every resource",
            Some(policy("codebuild:*", "*", None)),
            true,
        ),
        (
            "a role granted the action on every project of the account",
            Some(policy(
                "codebuild:StartBuild",
                &project_arn("*"),
                None,
            )),
            true,
        ),
        (
            "a role granted the object store only",
            Some(policy("s3:GetObject", "*", None)),
            false,
        ),
        (
            "a role granted the action on ANOTHER project",
            Some(policy(
                "codebuild:StartBuild",
                &project_arn("some-other-project"),
                None,
            )),
            false,
        ),
        (
            // IAM's deny precedence: an explicit `Deny` wins over any `Allow`.
            "a role granted the action and denied it",
            Some(policy(
                "codebuild:StartBuild",
                "*",
                Some("codebuild:StartBuild"),
            )),
            false,
        ),
        (
            // A role IAM holds no identity policy for says nothing about what it may do: an
            // execution role is routinely provisioned out of band.
            "a role that states no policy at all",
            None,
            true,
        ),
    ];

    for (index, (case, document, allowed)) in rows.iter().enumerate() {
        let project = format!("ba-project-{index}");
        let pipeline = format!("ba-pipeline-{index}");
        cb_ok(
            &router,
            case,
            "CreateProject",
            &json!({
                "name": project,
                "serviceRole": build_role,
                "source": {
                    "type": "CODEPIPELINE",
                    "buildspec": "version: 0.2\nphases:\n  build:\n    commands:\n      - echo ok\n",
                },
                "artifacts": {"type": "CODEPIPELINE"},
                "environment": {
                    "type": "LINUX_CONTAINER",
                    "image": "aws/codebuild/amazonlinux2-x86_64-standard:5.0",
                    "computeType": "BUILD_GENERAL1_SMALL",
                },
            }),
        )
        .await;
        let role = role_with(
            &router,
            &format!("ba-pipeline-role-{index}"),
            PIPELINE_TRUST,
            document.as_deref(),
        )
        .await;
        cp_ok(
            &router,
            case,
            "CreatePipeline",
            &declaration(&pipeline, &role, &project),
        )
        .await;

        let status = settled_execution(&router, case, &pipeline).await;
        let state = build_action_state(&router, case, &pipeline).await;
        let latest = state["latestExecution"].clone();
        // What CODEBUILD holds is what says whether the call was made: read through that
        // service's own listing, never out of the action's detail.
        let listed = cb_ok(
            &router,
            case,
            "ListBuildsForProject",
            &json!({"projectName": project}),
        )
        .await;
        let builds = listed["ids"].as_array().map(Vec::len).unwrap_or_default();

        if *allowed {
            assert_eq!(
                status, "Succeeded",
                "case {case:?}: expected the execution to succeed, got {status} with {latest}"
            );
            assert_eq!(
                builds, 1,
                "case {case:?}: the action must have started one build, got {listed}"
            );
            assert!(
                latest["externalExecutionId"].as_str().is_some(),
                "case {case:?}: the action must report the build it started, got {latest}"
            );
        } else {
            assert_eq!(
                status, "Failed",
                "case {case:?}: expected the execution to fail, got {status} with {latest}"
            );
            assert_eq!(
                builds, 0,
                "case {case:?}: no build may be created for a call the role may not make, \
                 got {listed}"
            );
            assert!(
                latest["externalExecutionId"].is_null(),
                "case {case:?}: an action that started nothing reports no external \
                 execution, got {latest}"
            );
            assert_eq!(
                latest["errorDetails"]["code"].as_str(),
                Some(JOB_FAILED),
                "case {case:?}: expected the {JOB_FAILED} failure type, got {latest}"
            );
            // The denial names the principal the call would have been made by, the action it
            // asked for and the project it asked for — AWS's own sentence for a request no
            // identity-based policy allows.
            let message = latest["errorDetails"]["message"]
                .as_str()
                .unwrap_or_default();
            for named in [
                role.as_str(),
                "codebuild:StartBuild",
                project_arn(&project).as_str(),
            ] {
                assert!(
                    message.contains(named),
                    "case {case:?}: the denial must name {named:?}, got {message:?}"
                );
            }
        }
    }
}
