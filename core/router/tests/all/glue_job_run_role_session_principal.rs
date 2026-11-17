// SPDX-License-Identifier: CC0-1.0

//! WHO a Glue job run's own AWS SDK calls are made by, across the services that decide it.
//!
//! RULE: a job run executes its script as the job's `Role` ("the name or Amazon Resource Name
//! (ARN) of the IAM role associated with this job", `Job$Role`), and the SESSION Glue opens of
//! that role for a run is opened under the role-session name Glue names a job runner's session
//! with — one fixed name, whatever the job is called — so the principal the run's calls speak
//! for is `arn:aws:sts::<account>:assumed-role/<role name>/GlueJobRunnerSession`. That is the
//! principal the token service answers the run's own `sts:GetCallerIdentity` with, and the
//! principal another service's authorization refusal of such a call blames; a run whose script
//! was answered that refusal comes to rest `FAILED` carrying the refusal AS THE SCRIPT'S OWN
//! SDK raised it, with nothing written where the refused call would have written; and the
//! grant the role was missing can be added, after which the same job runs to `SUCCEEDED`.
//!
//! Only a cross-service suite can pin this: the role and the policies in force on it are IAM's
//! records, the session is the token service's, the refusal is Amazon S3's own decision about
//! that session, and the run that reports it is Glue's. Each service's own crate can state its
//! half; none of them can state that the principal all four agree on is the job runner's
//! session.
//!
//! The rows differ along what the rule generalises over: the role the job runs as, the job's
//! own name (two jobs of ONE role, so a session named for the job would be told apart here),
//! the key the refused call addressed, and whether the policies in force allow that call at
//! all — plus the same job once the missing grant has been added.
//!
//! Every call goes over the wire into the router, the script and everything it writes travel
//! through Amazon S3's own operations, and each run is polled for the CONDITION that it has
//! come to rest against a generous deadline, so no outcome here depends on how fast the host
//! runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is addressed to.
const REGION: &str = "us-west-2";

/// The role-session name Glue opens a job run's session under, written out here rather than
/// read from the Glue crate's own constant so this suite pins it.
const RUNNER_SESSION: &str = "GlueJobRunnerSession";

/// The bucket the scripts, and everything the runs write, live in.
const BUCKET: &str = "xsvc-glue-runner-session";

/// The `JobRunState` values a run never leaves again
/// (`smithy_query(query="shape",shape="JobRunState",model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")`).
const TERMINAL_RUN_STATES: [&str; 6] = ["STOPPED", "SUCCEEDED", "FAILED", "TIMEOUT", "ERROR", "EXPIRED"];

/// How long a row waits for its run to come to rest, and how often it looks again. Generous:
/// nothing here asserts on how long anything took, and what is waited for is the run's own
/// state rather than a span.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The script a row runs: it writes `ok` at the key its `--output_key` argument names, which is
/// the call the row's role either may or may not make.
const WRITES_ITS_OUTPUT: &str = r#"
import sys

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
boto3.client("s3").put_object(
    Bucket=named["--bucket"], Key=named["--output_key"], Body=b"ok"
)
"#;

/// A script that writes the principal the run's OWN calls are made by, as the token service
/// answers it — so the session's identity is asserted positively and not only through a
/// refusal.
const WRITES_ITS_CALLER: &str = r#"
import sys

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
arn = boto3.client("sts").get_caller_identity()["Arn"]
boto3.client("s3").put_object(
    Bucket=named["--bucket"], Key=named["--output_key"], Body=arn.encode("utf-8")
)
"#;

/// The trust document a role Glue can assume carries.
const GLUE_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"glue.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// Glue, IAM, Amazon S3 and the token service on ONE engine: the script's own calls are
/// dispatched back into this same router, so the refusal a run reports is the decision this
/// suite's IAM records produced.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, without a computed signature.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDGLUERUNNER/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// What one call answered.
struct Answer {
    status: u16,
    body: String,
}

/// One request against a service's own host.
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Answer {
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// One IAM awsQuery action, which must be served.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = call(
        router,
        "iam",
        "iam.amazonaws.com",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        &body,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "IAM {action} must answer 200, got {} with {}",
        answered.status, answered.body
    );
    answered.body
}

/// One Amazon S3 REST call, path-style as the emulator serves it.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answer {
    call(
        router,
        "s3",
        &format!("s3.{REGION}.amazonaws.com"),
        method,
        path,
        &[("content-type", "application/octet-stream")],
        body,
    )
    .await
}

/// One AWS Glue awsJson1_1 operation, which must be served.
async fn glue(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let answered = call(
        router,
        "glue",
        &format!("glue.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("AWSGlue.{operation}")),
        ],
        &body.to_string(),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: Glue {operation} must answer 200, got {} with {}",
        answered.status, answered.body
    );
    serde_json::from_str(&answered.body).unwrap_or(Value::Null)
}

/// Create the suite's bucket.
async fn create_bucket(router: &MultiServiceRouter) {
    let created = s3(
        router,
        "PUT",
        &format!("/{BUCKET}"),
        &format!(
            "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
        ),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: the bucket must be created, got {} with {}",
        created.status, created.body
    );
}

/// Stage `source` as the script object of `job_name`, and answer the `s3://` location of it.
async fn stage_script(router: &MultiServiceRouter, job_name: &str, source: &str) -> String {
    let key = format!("scripts/{job_name}.py");
    let put = s3(router, "PUT", &format!("/{BUCKET}/{key}"), source).await;
    assert_eq!(
        put.status, 200,
        "setup: the script {key} must be stored, got {} with {}",
        put.status, put.body
    );
    format!("s3://{BUCKET}/{key}")
}

/// The body of one object, or `None` with the refusal the store answered the read with.
async fn object_body(
    router: &MultiServiceRouter,
    key: &str,
) -> Result<String, (String, String)> {
    let read = s3(router, "GET", &format!("/{BUCKET}/{key}"), "").await;
    if read.status == 200 {
        return Ok(read.body);
    }
    Err((
        element(&read.body, "Code"),
        element(&read.body, "Message"),
    ))
}

/// Create a role trusting Glue, carrying the statements `grants`, and answer its ARN.
async fn create_role(router: &MultiServiceRouter, role_name: &str, grants: &str) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", GLUE_TRUST),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    assert!(
        !arn.is_empty(),
        "setup: CreateRole must answer the role's ARN, got {created}"
    );
    put_role_policy(router, role_name, grants).await;
    arn
}

/// Put the statements `grants` on `role_name` as its one inline policy — the write that both
/// gives a role its grants and, run again, ADDS the one it was missing.
async fn put_role_policy(router: &MultiServiceRouter, role_name: &str, grants: &str) {
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", "job-run-access"),
            (
                "PolicyDocument",
                &format!(r#"{{"Version":"2012-10-17","Statement":[{grants}]}}"#),
            ),
        ],
    )
    .await;
}

/// The statement every role here carries: the READ of the script its job points at, so a row is
/// about the output write and never about the script read.
fn reads_the_script() -> String {
    format!(
        r#"{{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::{BUCKET}/scripts/*"}}"#
    )
}

/// The statement that admits the output write.
fn writes_the_output() -> String {
    format!(
        r#"{{"Effect":"Allow","Action":"s3:PutObject","Resource":"arn:aws:s3:::{BUCKET}/*"}}"#
    )
}

/// Create a `pythonshell` job named `job_name` running `script` as `role`.
async fn create_job(router: &MultiServiceRouter, job_name: &str, role: &str, script: &str) {
    let location = stage_script(router, job_name, script).await;
    glue(
        router,
        job_name,
        "CreateJob",
        &json!({
            "Name": job_name,
            "Role": role,
            "Command": {
                "Name": "pythonshell",
                "ScriptLocation": location,
                "PythonVersion": "3.9",
            },
            "MaxCapacity": 0.0625,
            "GlueVersion": "3.0",
        }),
    )
    .await;
}

/// Start one run of `job_name` writing `output_key`, poll it as a client does, and answer the
/// `JobRun` it reports once it has come to rest.
async fn run_writing(
    router: &MultiServiceRouter,
    case: &str,
    job_name: &str,
    output_key: &str,
) -> Value {
    let started = glue(
        router,
        case,
        "StartJobRun",
        &json!({
            "JobName": job_name,
            "Arguments": { "--bucket": BUCKET, "--output_key": output_key },
        }),
    )
    .await;
    let run_id = started["JobRunId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartJobRun must answer a JobRunId, got {started}"))
        .to_string();
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let reported = glue(
            router,
            case,
            "GetJobRun",
            &json!({ "JobName": job_name, "RunId": run_id }),
        )
        .await;
        let run = reported["JobRun"].clone();
        let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
        if TERMINAL_RUN_STATES.contains(&state.as_str()) {
            return run;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the run never came to rest; it reports {run}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The principal a session of `role_name` opened for a job run speaks for.
fn runner_principal(role_name: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role_name}/{RUNNER_SESSION}",
        ::engine::emulator_account_id()
    )
}

/// The sentence Amazon S3 refuses `principal` the write of `key` with, as the script's own SDK
/// renders it: botocore's `ClientError` carries the code the service answered, the operation it
/// called and that service's message, and Glue reports a script's uncaught exception as its
/// type and that sentence.
fn denied_put_object(role_name: &str, key: &str) -> String {
    format!(
        "ClientError: An error occurred (AccessDenied) when calling the PutObject operation: \
         User: {} is not authorized to perform: s3:PutObject on resource: \
         \"arn:aws:s3:::{BUCKET}/{key}\" because no identity-based policy allows the \
         s3:PutObject action",
        runner_principal(role_name)
    )
}

/// One row: the job and the role it runs as, and what the run must come to.
struct Row {
    /// The case name every assertion of the row reports.
    case: &'static str,
    /// The role the row's job runs as — two rows share one, so a session named for the JOB
    /// would be told apart by them.
    role_name: &'static str,
    /// The job's own name, which is not the name the session is opened under.
    job_name: &'static str,
    /// The key the script's write addresses.
    output_key: &'static str,
    /// Whether the policies in force on the role admit that write.
    allowed: bool,
}

/// The rows: one role refused the output write over two differently named jobs, another refused
/// it at another key, and one role allowed it.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role that may not write the output",
            role_name: "xsvc-glue-runner-denied",
            job_name: "xsvc_runner_denied",
            output_key: "results/bad.txt",
            allowed: false,
        },
        Row {
            case: "the same role under a differently named job",
            role_name: "xsvc-glue-runner-denied",
            job_name: "xsvc_runner_denied_other_job",
            output_key: "results/bad-again.txt",
            allowed: false,
        },
        Row {
            case: "another role refused the write at another key",
            role_name: "xsvc-glue-runner-elsewhere",
            job_name: "xsvc_runner_elsewhere",
            output_key: "elsewhere/output.bin",
            allowed: false,
        },
        Row {
            case: "a role that may write the output",
            role_name: "xsvc-glue-runner-allowed",
            job_name: "xsvc_runner_allowed",
            output_key: "results/good.txt",
            allowed: true,
        },
    ]
}

/// A run whose own call Amazon S3 refused reports that refusal as its script's SDK raised it,
/// naming the job run's role session as the principal — and leaves no object, so a read of the
/// key it would have written is the store's own miss.
#[tokio::test]
async fn a_refused_job_run_call_names_the_runs_role_session() {
    let router = build_router();
    create_bucket(&router).await;

    // The roles, created once each: two rows run as ONE role, which is what tells a session
    // named for the runner apart from one named for the job. A role is created with the grants
    // of the first row that names it, so rows sharing a role have to agree on whether the write
    // is allowed — asserted here rather than left to the order the table is read in.
    let mut roles: std::collections::BTreeMap<&str, (String, bool)> =
        std::collections::BTreeMap::new();
    for row in rows() {
        let case = row.case;
        let mut grants = reads_the_script();
        if row.allowed {
            grants.push(',');
            grants.push_str(&writes_the_output());
        }
        let role = match roles.get(row.role_name) {
            Some((held, allowed)) => {
                assert_eq!(
                    *allowed, row.allowed,
                    "case {case:?}: the rows sharing the role {:?} must agree on whether the \
                     write is allowed, and this one expects allowed={}",
                    row.role_name, row.allowed
                );
                held.clone()
            }
            None => {
                let created = create_role(&router, row.role_name, &grants).await;
                roles.insert(row.role_name, (created.clone(), row.allowed));
                created
            }
        };
        create_job(&router, row.job_name, &role, WRITES_ITS_OUTPUT).await;

        let run = run_writing(&router, case, row.job_name, row.output_key).await;
        let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
        let message = run["ErrorMessage"].as_str().unwrap_or_default().to_string();
        let written = object_body(&router, row.output_key).await;

        if row.allowed {
            assert_eq!(
                state, "SUCCEEDED",
                "case {case:?}: expected SUCCEEDED, got {run}"
            );
            assert!(
                run.get("ErrorMessage").is_none(),
                "case {case:?}: a run that succeeded carries no ErrorMessage, got {run}"
            );
            assert_eq!(
                written.as_deref(),
                Ok("ok"),
                "case {case:?}: expected the script's object, got {written:?}"
            );
            continue;
        }
        assert_eq!(
            state, "FAILED",
            "case {case:?}: expected FAILED, got {run}"
        );
        let expected = denied_put_object(row.role_name, row.output_key);
        assert_eq!(
            message, expected,
            "case {case:?}: expected ErrorMessage {expected:?}, got {message:?}"
        );
        // The refused write left nothing behind, so the object store's answer for that key is
        // its own miss: "NoSuchKey — The specified key does not exist." (Amazon S3 error
        // responses).
        assert_eq!(
            written,
            Err((
                "NoSuchKey".to_string(),
                "The specified key does not exist.".to_string()
            )),
            "case {case:?}: a refused write leaves no object, got {written:?}"
        );
    }
}

/// The grant a role was missing can be ADDED, and the SAME job then runs to its end: the
/// decision is taken against the documents in force when the run is made, and nothing of the
/// earlier refusal is remembered.
#[tokio::test]
async fn adding_the_missing_grant_lets_the_same_job_run_to_its_end() {
    let router = build_router();
    create_bucket(&router).await;

    let role_name = "xsvc-glue-runner-restored";
    let role = create_role(&router, role_name, &reads_the_script()).await;
    let job_name = "xsvc_runner_restored";
    create_job(&router, job_name, &role, WRITES_ITS_OUTPUT).await;

    let refused_key = "results/restore-first.txt";
    let before = run_writing(&router, "before the grant", job_name, refused_key).await;
    let expected = denied_put_object(role_name, refused_key);
    let message = before["ErrorMessage"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        message, expected,
        "case \"before the grant\": expected ErrorMessage {expected:?}, got {message:?}"
    );

    put_role_policy(
        &router,
        role_name,
        &format!("{},{}", reads_the_script(), writes_the_output()),
    )
    .await;
    let restored_key = "results/restore-second.txt";
    let after = run_writing(&router, "after the grant", job_name, restored_key).await;
    let state = after["JobRunState"].as_str().unwrap_or_default().to_string();
    assert_eq!(
        state, "SUCCEEDED",
        "case \"after the grant\": expected SUCCEEDED, got {after}"
    );
    let written = object_body(&router, restored_key).await;
    assert_eq!(
        written.as_deref(),
        Ok("ok"),
        "case \"after the grant\": expected the script's object, got {written:?}"
    );
}

/// The principal a job run's own calls speak for, read back POSITIVELY: the token service
/// answers the run's own `sts:GetCallerIdentity` with the session of the job's role, opened
/// under the name Glue opens a job runner's session under — the same name for two jobs of one
/// role.
#[tokio::test]
async fn a_job_run_calls_as_a_session_of_its_role_named_for_the_runner() {
    let router = build_router();
    create_bucket(&router).await;

    let role_name = "xsvc-glue-runner-identity";
    let role = create_role(
        &router,
        role_name,
        &format!("{},{}", reads_the_script(), writes_the_output()),
    )
    .await;
    for (case, job_name, output_key) in [
        ("the first job", "xsvc_runner_identity_one", "identity/one.txt"),
        ("the second job", "xsvc_runner_identity_two", "identity/two.txt"),
    ] {
        create_job(&router, job_name, &role, WRITES_ITS_CALLER).await;
        let run = run_writing(&router, case, job_name, output_key).await;
        let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            state, "SUCCEEDED",
            "case {case:?}: expected SUCCEEDED, got {run}"
        );
        let reported = object_body(&router, output_key).await;
        assert_eq!(
            reported.as_deref(),
            Ok(runner_principal(role_name).as_str()),
            "case {case:?}: expected the run's own caller identity, got {reported:?}"
        );
    }
}
