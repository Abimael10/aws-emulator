// SPDX-License-Identifier: CC0-1.0

//! WHERE a build's output really goes: the log events the group and stream a build reports
//! hold afterwards, read back through CloudWatch Logs' own reads.
//!
//! RULE under test: a build's output is DELIVERED to the CloudWatch Logs group and stream the
//! build reports — which is what makes `logs.groupName`/`logs.streamName` the documented way
//! to read WHY a build ended as it did: a client reads the location off the build record and
//! then reads the events there. What is delivered is what the build's commands wrote, on both
//! of their streams: the lines a command wrote on standard output, and the diagnostic a
//! command that could not do its work wrote on standard error. A build whose service role may
//! not `logs:PutLogEvents` into that stream still runs and still reports where its output
//! would have gone, and delivers no events.
//!
//! Two services have to agree for that to hold — CloudWatch Logs holds the group, IAM holds
//! the role's policies — so this suite mounts both beside CodeBuild on ONE engine, the way
//! the assembled emulator does, and reads the events back through CloudWatch Logs rather than
//! out of CodeBuild.
//!
//! The rows differ along what the rule generalises over: whether the project configures a
//! group and a stream prefix of its own, what the build's commands wrote and on which stream,
//! and whether the build's role may deliver at all.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::project_build_lifecycle::REGION;

/// CloudWatch Logs' `X-Amz-Target` prefix, from that service's own OPERATIONS.md.
const LOGS_TARGET: &str = "Logs_20140328";

/// CodeBuild's `X-Amz-Target` prefix, from that service's own OPERATIONS.md.
const CODEBUILD_TARGET: &str = "CodeBuild_20161006";

/// The trust policy a build project's service role carries.
const CODEBUILD_TRUST: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
"Principal":{"Service":"codebuild.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The actions AWS documents a build's service role needs to deliver its log events (the
/// CodeBuild service-role policy its User Guide publishes).
const LOG_ACTIONS: [&str; 3] = [
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// How long a wait for a build to settle may take before the suite calls it stalled.
/// Generous on purpose: the wait ends on the CONDITION, and the deadline is only there so a
/// build that never settles fails the test instead of hanging it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the wait below leaves before it looks again.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// One emulator carrying IAM, CloudWatch Logs and CodeBuild over ONE shared engine.
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
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(engine),
    );
    router
}

/// One awsJson1_1 call to the service `signing_name` claims, under `target`.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=abc"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the test body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let decoded = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// One CodeBuild operation that has to succeed.
async fn codebuild(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answer) = json_call(router, "codebuild", CODEBUILD_TARGET, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: codebuild {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One CloudWatch Logs operation, with whatever it answered.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    json_call(router, "logs", LOGS_TARGET, operation, body).await
}

/// One value, percent-encoded for the form body an awsQuery call carries.
fn form_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
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
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Create a role trusted by CodeBuild whose inline policy allows `actions`, and answer the
/// ARN IAM holds it under.
async fn role_allowing(router: &MultiServiceRouter, name: &str, actions: &[&str]) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", CODEBUILD_TRUST),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateRole {name:?}: expected the role to be persisted, got {status}: {created}"
    );
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": actions, "Resource": "*"}],
    })
    .to_string();
    let (status, attached) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "build-permissions"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "PutRolePolicy {name:?}: expected the policy to be persisted, got {status}: {attached}"
    );
    format!(
        "arn:aws:iam::{}:role/{name}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// Start a build of `project` and look again until the transition the clock owes it has
/// fallen due, answering the settled record `BatchGetBuilds` reports.
async fn settled_build(router: &MultiServiceRouter, case: &str, project: &str) -> Value {
    let started = codebuild(router, case, "StartBuild", &json!({"projectName": project})).await;
    let id = started["build"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the start must report a build id"))
        .to_string();
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = codebuild(router, case, "BatchGetBuilds", &json!({"ids": [id.clone()]})).await;
        let build = read["builds"][0].clone();
        if build["buildComplete"] == json!(true) {
            return build;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the build {id} must settle, last read {build}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The messages the stream `stream` of the group `group` holds, as CloudWatch Logs' own read
/// reports them — and `None` when it holds no such stream at all.
async fn delivered_messages(
    router: &MultiServiceRouter,
    group: &str,
    stream: &str,
) -> Option<Vec<String>> {
    let (status, read) = logs(
        router,
        "GetLogEvents",
        &json!({"logGroupName": group, "logStreamName": stream}),
    )
    .await;
    if status != 200 {
        return None;
    }
    Some(
        read["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|event| event["message"].as_str().map(str::to_string))
            .collect(),
    )
}

/// A `CreateProject` request for `name` running as `role`, with `buildspec` and `extra`'s
/// members.
fn project_body(name: &str, role: &str, buildspec: &str, extra: Value) -> Value {
    let mut body = json!({
        "name": name,
        "serviceRole": role,
        "source": {"type": "NO_SOURCE", "buildspec": buildspec},
        "artifacts": {"type": "NO_ARTIFACTS"},
        "environment": {
            "type": "LINUX_CONTAINER",
            "image": "aws/codebuild/standard:7.0",
            "computeType": "BUILD_GENERAL1_SMALL",
        },
    });
    if let (Value::Object(target), Value::Object(members)) = (&mut body, extra) {
        target.extend(members);
    }
    body
}

/// A buildspec whose build phase runs `commands`.
fn buildspec(commands: &[&str]) -> String {
    let listed = commands
        .iter()
        .map(|command| format!("      - '{command}'\n"))
        .collect::<String>();
    format!("version: 0.2\nphases:\n  build:\n    commands:\n{listed}")
}

/// RULE: a build's output is delivered to the group and stream the build reports.
#[tokio::test]
async fn a_builds_output_is_delivered_where_the_build_says_it_went() {
    let router = emulator();
    let role = role_allowing(&router, "codebuild-logs-role", &LOG_ACTIONS).await;

    /// One row: the case, the extra `CreateProject` members, the commands the build runs, and
    /// the lines its stream must then hold.
    type Row = (
        &'static str,
        Value,
        Vec<&'static str>,
        Vec<&'static str>,
    );

    let rows: Vec<Row> = vec![
        (
            "a build whose commands write to standard output",
            json!({}),
            vec!["echo building the thing", "echo and another line"],
            vec!["building the thing", "and another line"],
        ),
        (
            "a project that configures its own group and stream prefix",
            json!({"logsConfig": {"cloudWatchLogs": {
                "status": "ENABLED",
                "groupName": "own-build-group",
                "streamName": "own-prefix",
            }}}),
            vec!["echo into the configured group"],
            vec!["into the configured group"],
        ),
        (
            // A utility that could not do its work writes on standard ERROR, and a build's
            // log carries that stream too.
            "a build whose command reports what it could not do",
            json!({}),
            vec!["echo before", "cat /no/such/file"],
            vec!["before", "cat: /no/such/file: No such file or directory"],
        ),
    ];

    for (index, (case, extra, commands, expected)) in rows.iter().enumerate() {
        let project = format!("log-proj-{index}");
        codebuild(
            &router,
            case,
            "CreateProject",
            &project_body(&project, &role, &buildspec(commands), extra.clone()),
        )
        .await;
        let build = settled_build(&router, case, &project).await;
        let group = build["logs"]["groupName"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the build must report its group, got {build}"))
            .to_string();
        let stream = build["logs"]["streamName"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the build must report its stream, got {build}")
            })
            .to_string();
        let messages = delivered_messages(&router, &group, &stream)
            .await
            .unwrap_or_else(|| {
                panic!("case {case:?}: CloudWatch Logs must hold {group}/{stream}")
            });
        for line in expected {
            assert!(
                messages.iter().any(|message| message == line),
                "case {case:?}: expected {line:?} among the events of {group}/{stream}, \
                 got {messages:?}"
            );
        }
    }
}

/// RULE: what a build's TOOLS could not do is in its log too — the AWS CLI's own line for a
/// call the build's role may not make, which is how a client reads WHY such a build failed.
#[tokio::test]
async fn a_denied_call_is_reported_in_the_builds_log() {
    let router = emulator();
    // A role that states permissions and allows no Amazon ECR action: the registry call the
    // build's command makes is refused, and the CLI reports that.
    let role = role_allowing(&router, "codebuild-logs-noecr", &LOG_ACTIONS).await;
    let case = "a build whose role may not get a registry token";
    codebuild(
        &router,
        case,
        "CreateProject",
        &project_body(
            "log-denied-proj",
            &role,
            &buildspec(&["aws ecr get-login-password --region $AWS_REGION"]),
            json!({}),
        ),
    )
    .await;
    let build = settled_build(&router, case, "log-denied-proj").await;
    assert_eq!(
        build["buildStatus"], json!("FAILED"),
        "case {case:?}: a build whose command was refused fails, got {build}"
    );
    let group = build["logs"]["groupName"].as_str().unwrap_or_default();
    let stream = build["logs"]["streamName"].as_str().unwrap_or_default();
    let messages = delivered_messages(&router, group, stream)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: CloudWatch Logs must hold {group}/{stream}"));
    let reported = messages.join("\n");
    // The CLI's own sentence for a denied call, naming the operation, the session the build
    // acts in, the action and the resource.
    for named in [
        "An error occurred (AccessDeniedException) when calling the GetAuthorizationToken \
         operation",
        "is not authorized to perform: ecr:GetAuthorizationToken on resource: *",
        "AWSCodeBuild-",
    ] {
        assert!(
            reported.contains(named),
            "case {case:?}: expected {named:?} in the build's log, got {messages:?}"
        );
    }
}

/// RULE: a build whose service role may not put log events into its stream still runs and
/// still reports where its output would have gone — and delivers none.
#[tokio::test]
async fn a_build_whose_role_may_not_deliver_delivers_no_events() {
    let router = emulator();
    let role = role_allowing(&router, "codebuild-logs-denied", &["s3:GetObject"]).await;
    let case = "a role that states permissions and allows no log delivery";
    codebuild(
        &router,
        case,
        "CreateProject",
        &project_body(
            "log-undelivered-proj",
            &role,
            &buildspec(&["echo nothing reaches the group"]),
            json!({}),
        ),
    )
    .await;
    let build = settled_build(&router, case, "log-undelivered-proj").await;
    assert_eq!(
        build["buildStatus"], json!("SUCCEEDED"),
        "case {case:?}: the build still runs, got {build}"
    );
    let group = build["logs"]["groupName"].as_str().unwrap_or_default();
    let stream = build["logs"]["streamName"].as_str().unwrap_or_default();
    assert!(
        !group.is_empty() && !stream.is_empty(),
        "case {case:?}: the build still reports where its output would have gone, got {build}"
    );
    // Nothing was delivered: CloudWatch Logs holds no such stream, or an empty one.
    let messages = delivered_messages(&router, group, stream).await;
    assert!(
        messages.as_ref().is_none_or(Vec::is_empty),
        "case {case:?}: expected no delivered events, got {messages:?}"
    );
}
