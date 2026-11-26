// SPDX-License-Identifier: CC0-1.0

//! Whether a Glue `pythonshell` job run is dispatched at all is decided by the TRUST policy of
//! the role the job runs as, across the three services that decide it.
//!
//! RULE: a job run is performed BY Glue acting as the job's `Role` ("the name or Amazon Resource
//! Name (ARN) of the IAM role associated with this job", `Job$Role`), so a run is dispatched only
//! while this account's own IAM shows that the Glue service principal may assume that role. A run
//! whose role admits it comes to rest in the terminal `SUCCEEDED` state carrying no
//! `JobRun$ErrorMessage`, and the object its script writes is really there; a run whose role does
//! not comes to rest in the terminal `FAILED` state carrying a non-empty `ErrorMessage` of the
//! role-assumption class — the assume-role requirement for the role the job names — and nothing
//! of the script runs, so the object it would have written is not there. Neither run is left in a
//! non-terminal state: a client that polls `GetJobRun` reaches a terminal `JobRunState` either
//! way
//! (`smithy_query(query="shape",shape="JobRunState",model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")`).
//!
//! Only a cross-service suite can pin this: the role and its trust document are IAM's own
//! records, written through `CreateRole` and `AttachRolePolicy` on IAM's own
//! `application/x-www-form-urlencoded` wire; the script and the object it writes are Amazon S3's;
//! and the run that reports the outcome is Glue's. Each crate can state its own half; none of
//! them can state that the three agree.
//!
//! What is held CONSTANT across the rows is the permissions side of the role: every row's role
//! carries exactly one policy, the AWS-managed `AWSGlueServiceRole`, attached by ARN. That policy
//! grants `s3:GetObject`/`s3:PutObject` on `arn:aws:s3:::aws-glue-*/*`, which is why this suite's
//! bucket is named with that prefix — so the only thing a row varies is what its role's trust
//! document says, and a run that is not dispatched is not one that could not read its script.
//!
//! The rows differ along what the rule generalises over: which principal the trust document
//! admits (the Glue service principal, another service, several services at once, the account
//! root rather than a service), which action it names for that principal (the assume itself, a
//! wildcard covering it, an action that is not the assume), and the effect it states (an `Allow`,
//! an explicit `Deny`). The recorded case — one role trusting `glue.amazonaws.com` beside one
//! trusting `ec2.amazonaws.com` — is two of them.
//!
//! Every run is started first and then polled for the CONDITION that it has come to rest, against
//! a generous deadline, and nothing here asserts on how long anything took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is addressed to.
const REGION: &str = "us-west-2";

/// The bucket the scripts, and the markers the scripts write, live in.
///
/// Named with the prefix the AWS-managed Glue service-role policy grants object access on
/// (`arn:aws:s3:::aws-glue-*/*`), so every row's role may read its script and write its marker
/// and the rows differ only in what their roles are TRUSTED for.
const BUCKET: &str = "aws-glue-xsvc-pythonshell-trust";

/// The one policy every row's role carries, by the ARN IAM publishes it under.
const MANAGED_POLICY: &str = "arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole";

/// The service principal Glue has to be admitted as to assume a job's execution role, and the
/// action a trust document grants it — written out here rather than read from the Glue crate's
/// own constants so this suite pins them.
const GLUE_PRINCIPAL: &str = "glue.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The `JobRunState` values a run never leaves again
/// (`smithy_query(query="shape",shape="JobRunState",model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")`).
const TERMINAL_RUN_STATES: [&str; 6] = [
    "STOPPED",
    "SUCCEEDED",
    "FAILED",
    "TIMEOUT",
    "ERROR",
    "EXPIRED",
];

/// How long a row waits for its run to come to rest, and how long it waits before looking again.
/// Generous: what is waited for is the run's own state, and no assertion reads elapsed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The body every row's script writes, which is what makes the marker object non-empty.
const MARKER_BODY: &str = "ok";

/// The script a row stages: it writes [`MARKER_BODY`] at the key its `--marker` argument names,
/// so a row can tell a run that EXECUTED from one that never reached the interpreter.
const WRITES_ITS_MARKER: &str = r#"
import sys

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
boto3.client("s3").put_object(
    Bucket=named["--bucket"], Key=named["--marker"], Body=b"ok"
)
"#;

/// Glue, IAM and Amazon S3 on ONE engine: the run resolves its role against the records this
/// suite's IAM calls wrote, and the script is read from — and writes back into — this same
/// object store.
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
        engine,
    ));
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDGLUETRUST/20260101/{REGION}/{service}/aws4_request, \
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
        "setup: IAM {action} must answer 200, got {} with {}",
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

/// A trust document with one statement: `effect` for the principal element `principal`
/// performing `action`.
fn trust(effect: &str, principal: Value, action: Value) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": principal,
            "Action": action,
        }],
    })
    .to_string()
}

/// A trust document naming SERVICE principals.
fn service_trust(effect: &str, services: Value, action: Value) -> String {
    trust(effect, json!({ "Service": services }), action)
}

/// What a row expects of the run of a job carrying its role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The role admits Glue: the script ran, the run succeeded and the marker is there.
    Ran,
    /// The role does not: the run failed carrying the assume-role requirement, and nothing ran.
    RoleNotAssumable,
}

/// One row: the label its resources are named for, the trust document its role carries, and what
/// the run of a job carrying that role must come to.
struct Row {
    /// The case name every assertion of the row reports.
    case: &'static str,
    /// The label the row's role, job, script key and marker key are named for.
    label: &'static str,
    /// The trust document the account holds for that role.
    trust_document: String,
    /// What the run must come to.
    expected: Expected,
}

/// The rows: every form of trust document, against what the run of a job carrying that role
/// comes to.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role that trusts the Glue service principal",
            label: "good",
            trust_document: service_trust("Allow", json!(GLUE_PRINCIPAL), json!(ASSUME_ROLE)),
            expected: Expected::Ran,
        },
        Row {
            case: "a role that trusts another service",
            label: "bad",
            trust_document: service_trust("Allow", json!("ec2.amazonaws.com"), json!(ASSUME_ROLE)),
            expected: Expected::RoleNotAssumable,
        },
        Row {
            case: "a role that trusts Glue beside another service",
            label: "both",
            trust_document: service_trust(
                "Allow",
                json!(["ec2.amazonaws.com", GLUE_PRINCIPAL]),
                json!(ASSUME_ROLE),
            ),
            expected: Expected::Ran,
        },
        Row {
            case: "a role that trusts Glue through a wildcard over the token service's actions",
            label: "wildcard",
            trust_document: service_trust("Allow", json!(GLUE_PRINCIPAL), json!("sts:*")),
            expected: Expected::Ran,
        },
        Row {
            case: "a role whose statement names Glue under another action",
            label: "other_action",
            trust_document: service_trust("Allow", json!(GLUE_PRINCIPAL), json!("s3:GetObject")),
            expected: Expected::RoleNotAssumable,
        },
        Row {
            case: "a role that explicitly denies Glue the assume",
            label: "denied",
            trust_document: service_trust("Deny", json!(GLUE_PRINCIPAL), json!(ASSUME_ROLE)),
            expected: Expected::RoleNotAssumable,
        },
        Row {
            case: "a role that trusts the account root rather than a service",
            label: "account_root",
            trust_document: trust(
                "Allow",
                json!({ "AWS": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id()) }),
                json!(ASSUME_ROLE),
            ),
            expected: Expected::RoleNotAssumable,
        },
    ]
}

/// One started row: the names its assertions address, and the run that was started.
struct Started {
    case: &'static str,
    job_name: String,
    run_id: String,
    role: String,
    marker_key: String,
    expected: Expected,
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

/// Stage one row's role, script and job, and start a run of it.
async fn start_row(router: &MultiServiceRouter, row: &Row) -> Started {
    let case = row.case;
    let script_key = format!("scripts/{}.py", row.label);
    let put = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/{script_key}"),
        WRITES_ITS_MARKER,
    )
    .await;
    assert_eq!(
        put.status, 200,
        "case {case:?}: the script {script_key} must be stored, got {} with {}",
        put.status, put.body
    );

    let role_name = format!("xsvc-glue-trust-{}", row.label);
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", &role_name),
            ("AssumeRolePolicyDocument", &row.trust_document),
        ],
    )
    .await;
    let role = element(&created, "Arn");
    assert!(
        !role.is_empty(),
        "case {case:?}: CreateRole must answer the role's ARN, got {created}"
    );
    iam(
        router,
        "AttachRolePolicy",
        &[("RoleName", &role_name), ("PolicyArn", MANAGED_POLICY)],
    )
    .await;

    let job_name = format!("xsvc_glue_trust_{}", row.label);
    let marker_key = format!("markers/{}.txt", row.label);
    glue(
        router,
        case,
        "CreateJob",
        &json!({
            "Name": job_name,
            "Role": role,
            "Command": {
                "Name": "pythonshell",
                "PythonVersion": "3.9",
                "ScriptLocation": format!("s3://{BUCKET}/{script_key}"),
            },
            "MaxCapacity": 0.0625,
            "Timeout": 5,
        }),
    )
    .await;

    let started = glue(
        router,
        case,
        "StartJobRun",
        &json!({
            "JobName": job_name,
            "Arguments": { "--bucket": BUCKET, "--marker": marker_key },
        }),
    )
    .await;
    let run_id = started["JobRunId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: StartJobRun must answer a JobRunId, got {started}")
        })
        .to_string();
    Started {
        case,
        job_name,
        run_id,
        role,
        marker_key,
        expected: row.expected,
    }
}

/// Poll `GetJobRun` until the run has come to rest, exactly as a client waits for one, leaving
/// the due instants of its lifecycle where the service put them.
async fn run_at_rest(router: &MultiServiceRouter, started: &Started) -> Value {
    let case = started.case;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let reported = glue(
            router,
            case,
            "GetJobRun",
            &json!({ "JobName": started.job_name, "RunId": started.run_id }),
        )
        .await;
        let run = reported["JobRun"].clone();
        let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
        if TERMINAL_RUN_STATES.contains(&state.as_str()) {
            return run;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the run must come to rest, and it reports {run}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A `pythonshell` run is dispatched only while IAM shows the Glue service principal may assume
/// the job's role: the admitted rows execute their script, come to rest `SUCCEEDED` and leave
/// their marker object behind, and the refused rows come to rest `FAILED` carrying the
/// assume-role requirement for the role the job names, with nothing of the script run.
#[tokio::test]
async fn a_pythonshell_run_is_dispatched_only_for_a_role_that_trusts_glue() {
    let router = build_router();
    create_bucket(&router).await;

    // Every row is started FIRST and polled afterwards, so the runs of the whole table are in
    // flight together rather than one lifecycle after another.
    let mut started: Vec<Started> = Vec::new();
    for row in rows() {
        started.push(start_row(&router, &row).await);
    }

    for row in &started {
        let case = row.case;
        let run = run_at_rest(&router, row).await;
        let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
        let message = run["ErrorMessage"].as_str().unwrap_or_default().to_string();
        let marker = s3(&router, "GET", &format!("/{BUCKET}/{}", row.marker_key), "").await;

        match row.expected {
            Expected::Ran => {
                assert_eq!(
                    state, "SUCCEEDED",
                    "case {case:?}: expected SUCCEEDED, got {run}"
                );
                assert!(
                    run.get("ErrorMessage").is_none(),
                    "case {case:?}: a run that succeeded carries no ErrorMessage, got {run}"
                );
                assert_eq!(
                    marker.status, 200,
                    "case {case:?}: the script's marker must be served, got {} with {}",
                    marker.status, marker.body
                );
                assert_eq!(
                    marker.body, MARKER_BODY,
                    "case {case:?}: expected the marker {MARKER_BODY:?}, got {:?}",
                    marker.body
                );
            }
            Expected::RoleNotAssumable => {
                assert_eq!(
                    state, "FAILED",
                    "case {case:?}: expected FAILED, got {run}"
                );
                // `JobRun$ErrorMessage` is "an error message associated with this job run": a run
                // nothing dispatched carries the assume-role requirement for the role it would
                // have run as, so the message is non-empty and names that role.
                assert!(
                    !message.is_empty(),
                    "case {case:?}: a run that failed carries an ErrorMessage, got {run}"
                );
                assert!(
                    message.contains(&row.role),
                    "case {case:?}: expected the ErrorMessage to name the role {:?}, got \
                     {message:?}",
                    row.role
                );
                assert!(
                    message.to_lowercase().contains("assume role"),
                    "case {case:?}: expected the ErrorMessage to state the assume-role \
                     requirement, got {message:?}"
                );
                // Nothing of this emulator's own internals reaches the modelled member.
                for leak in ["SdkBody", "ServiceError", "StatusCode(", "Unhandled"] {
                    assert!(
                        !message.contains(leak),
                        "case {case:?}: ErrorMessage must carry no {leak:?}, got {message:?}"
                    );
                }
                // The script never ran, so the object store holds no marker for this row:
                // "NoSuchKey — The specified key does not exist." (Amazon S3 error responses).
                assert_eq!(
                    marker.status, 404,
                    "case {case:?}: nothing must have run, but the marker read answered {} with \
                     {}",
                    marker.status, marker.body
                );
            }
        }
    }
}
