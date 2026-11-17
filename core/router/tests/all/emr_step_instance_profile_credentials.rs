// SPDX-License-Identifier: CC0-1.0

//! What a step's work HOLDS: the credential the cluster's Amazon EC2 instances obtain from
//! the instance profile the launch named.
//!
//! The RULE under test: **"the Amazon EC2 instances of the cluster assume this role"
//! (`RunJobFlow$JobFlowRole`) through the instance profile the launch names, and an instance
//! assumes a role only where that role's trust policy admits the Amazon EC2 service
//! principal — so a launch naming an instance profile that resolves is ACCEPTED whatever its
//! role trusts, and a profile whose role trusts another service leaves that cluster's
//! instances with NO credential: the work of a step that needs one does not get it, so the
//! step FAILS and the object it would have written is absent, while the same work on a
//! cluster whose profile's role admits Amazon EC2 completes and its object is there.**
//!
//! The trust mismatch is therefore no API-time refusal: `RunJobFlow` answers 200 with a
//! `JobFlowId` for every row below. Which refusals the profile itself earns at the API —
//! a value naming no profile of this account, and a profile carrying no role at all — is
//! `emr_job_flow_role_instance_profile.rs`'s rule.
//!
//! Only a cross-service suite can pin this: IAM holds the profiles and the roles whose trust
//! decides the credential, Amazon EMR runs the step, and the object the step's command line
//! carries is a real object of Amazon S3's own store — read back here through `GetObject`,
//! never through anything this suite handed Amazon EMR. It therefore runs against the WHOLE
//! `build_router` service set, as the binary serves it.
//!
//! The rows differ along what the rule generalises over: WHICH service principal the
//! profile's role trusts (Amazon EC2, and two other services, so nothing is keyed to one
//! name) and WHAT the step's work needs the credential for (writing an object, and reading
//! one and writing it back).

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: the one whose `CreateBucket` needs no
/// location constraint in its body.
const REGION: &str = "us-east-1";

/// The bucket every step of this suite reads from and writes to.
const BUCKET: &str = "emr-instance-profile-credentials";

/// The object the rows that READ one read, and the bytes it holds.
const SEED_KEY: &str = "in/seed.txt";
const SEED_BYTES: &str = "seeded\n";

/// What a step's command line writes into its marker, which is what the marker then holds.
const MARKER_BYTES: &str = "ok\n";

/// The service role every launch names — "the IAM role that Amazon EMR assumes in order to
/// access Amazon Web Services resources on your behalf" (`RunJobFlow$ServiceRole`).
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// The service principal an Amazon EC2 instance assumes a role as, which is what a role an
/// instance profile carries has to trust for the instance to obtain a credential for it.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// `FailureDetails$Reason` of a step whose command exited non-zero, spelled out here rather
/// than imported from the service crate, so a wrong constant in the implementation cannot
/// make this suite pass.
const COMMAND_FAILED_REASON: &str = "The command the step submitted exited with a non-zero status.";

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
const TERMINAL: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// How long a poll waits for a step to settle: both provisioning phases, the step's own span
/// and one command's transfers, with room to spare. The wait is on the CONDITION — it looks
/// again until the step has settled — so a loaded host delays a row instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(180);

/// How long a poll sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The whole emulator over one engine, exactly as `core/router/src/lib.rs` assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: the router above runs with authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One Amazon EMR call (`awsJson1_1`, `ElasticMapReduce.<Operation>`): its status and body.
async fn emr(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("elasticmapreduce.{REGION}.amazonaws.com"))
        .header("authorization", authorization("elasticmapreduce"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("ElasticMapReduce.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon EMR call that must be served.
async fn emr_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = emr(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One IAM `awsQuery` call, which must answer 200.
async fn iam(router: &MultiServiceRouter, case: &str, body: &str) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body.to_owned()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM, got {status} with {text}"
    );
    text
}

/// One path-style Amazon S3 request: its status and body.
async fn s3(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .body(Bytes::copy_from_slice(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// Create the bucket every row uses, and seed the object the reading rows read.
async fn seed_store(router: &MultiServiceRouter) {
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "setup: CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/{SEED_KEY}"),
        SEED_BYTES.as_bytes(),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "setup: PutObject {SEED_KEY} must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// The object at `key`, or `None` when the store holds none.
async fn object(router: &MultiServiceRouter, key: &str) -> Option<String> {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    (200..300)
        .contains(&status)
        .then(|| String::from_utf8_lossy(&body).to_string())
}

/// A trust policy admitting `principal` and nothing else, in the form an `awsQuery` form body
/// carries it: its quotes and spaces percent-encoded.
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
    .replace('"', "%22")
    .replace(' ', "%20")
}

/// The service role every launch names, created once for the whole suite.
async fn create_service_role(router: &MultiServiceRouter, case: &str) {
    iam(
        router,
        case,
        &format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={SERVICE_ROLE}\
             &AssumeRolePolicyDocument={}",
            trust_policy("elasticmapreduce.amazonaws.com")
        ),
    )
    .await;
}

/// An instance profile called `name` carrying a role of the same name whose trust policy
/// admits `principal`, built the way AWS builds one: `CreateRole`, `CreateInstanceProfile`,
/// `AddRoleToInstanceProfile`.
async fn create_instance_profile(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    principal: &str,
) {
    iam(
        router,
        case,
        &format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}&AssumeRolePolicyDocument={}",
            trust_policy(principal)
        ),
    )
    .await;
    iam(
        router,
        case,
        &format!(
            "Action=CreateInstanceProfile&Version=2010-05-08&InstanceProfileName={name}&Path=/"
        ),
    )
    .await;
    iam(
        router,
        case,
        &format!(
            "Action=AddRoleToInstanceProfile&Version=2010-05-08\
             &InstanceProfileName={name}&RoleName={name}"
        ),
    )
    .await;

    // Round trip: IAM reports the profile carrying that role, so each row's premise is read
    // back through IAM's own reader rather than assumed.
    let held = iam(
        router,
        case,
        &format!("Action=GetInstanceProfile&Version=2010-05-08&InstanceProfileName={name}"),
    )
    .await;
    assert!(
        held.contains(&format!("<RoleName>{name}</RoleName>")),
        "case {case:?}: GetInstanceProfile {name} must report the role it carries, got {held}"
    );
}

/// What the step's command line needs the instance's credential FOR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Work {
    /// It writes a file and uploads it.
    Upload,
    /// It downloads the seeded object, reads it back and uploads what it read.
    ReadBackAndUpload,
}

/// One row: the service principal the cluster's instance-profile role trusts, the work its
/// one step submits, and whether that step completes.
struct Row {
    case: &'static str,
    principal: &'static str,
    work: Work,
    /// Whether the cluster's instances obtain a credential, so the work can be done.
    completes: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a profile whose role trusts Amazon EC2, uploading a file it wrote",
            principal: EC2_PRINCIPAL,
            work: Work::Upload,
            completes: true,
        },
        Row {
            case: "a profile whose role trusts Amazon EC2, reading an object and writing it back",
            principal: EC2_PRINCIPAL,
            work: Work::ReadBackAndUpload,
            completes: true,
        },
        Row {
            case: "a profile whose role trusts AWS Glue, uploading a file it wrote",
            principal: "glue.amazonaws.com",
            work: Work::Upload,
            completes: false,
        },
        Row {
            case: "a profile whose role trusts AWS Lambda, uploading a file it wrote",
            principal: "lambda.amazonaws.com",
            work: Work::Upload,
            completes: false,
        },
        Row {
            case: "a profile whose role trusts AWS Glue, reading an object and writing it back",
            principal: "glue.amazonaws.com",
            work: Work::ReadBackAndUpload,
            completes: false,
        },
    ]
}

/// The marker key one row's step writes.
fn marker_key(ordinal: usize) -> String {
    format!("proof/marker-{ordinal}.txt")
}

/// The command line one row's step submits, as a shell program after `-c` — the form the
/// CLI's `--steps` writes.
fn command_line(row: &Row, ordinal: usize) -> Vec<String> {
    let marker = marker_key(ordinal);
    let program = match row.work {
        Work::Upload => format!(
            "echo ok > /tmp/marker.txt && aws s3 cp /tmp/marker.txt s3://{BUCKET}/{marker}"
        ),
        Work::ReadBackAndUpload => format!(
            "aws s3 cp s3://{BUCKET}/{SEED_KEY} /tmp/seed.txt && \
             cat /tmp/seed.txt > /tmp/marker.txt && \
             aws s3 cp /tmp/marker.txt s3://{BUCKET}/{marker}"
        ),
    };
    vec!["bash".to_string(), "-c".to_string(), program]
}

/// What one row's marker must hold once its step has completed.
fn expected_marker(row: &Row) -> &'static str {
    match row.work {
        Work::Upload => MARKER_BYTES,
        Work::ReadBackAndUpload => SEED_BYTES,
    }
}

/// Poll `ListSteps` until every step of `cluster_id` is terminal, answering them in
/// submission order.
async fn drained(case: &str, router: &MultiServiceRouter, cluster_id: &str) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let listed = emr_ok(router, case, "ListSteps", &json!({ "ClusterId": cluster_id })).await;
        let mut steps: Vec<Value> = listed["Steps"].as_array().cloned().unwrap_or_default();
        steps.reverse();
        let states: Vec<String> = steps
            .iter()
            .map(|step| {
                step["Status"]["State"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        if !states.is_empty() && states.iter().all(|state| TERMINAL.contains(&state.as_str())) {
            return steps;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected every step to reach a terminal state, got {states:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_steps_work_holds_only_what_its_instance_profiles_role_lets_the_instances_obtain() {
    let router = router();
    let rows = rows();

    seed_store(&router).await;
    create_service_role(&router, "setup").await;

    // One profile and one cluster per row, so each row's step is answered on a cluster of its
    // own. Every launch is accepted — the trust mismatch is no API-time refusal — and the
    // clusters are launched before any step is polled, so their spans run together.
    let mut launched: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let profile = format!("EMR_EC2_Profile_{ordinal}");
        create_instance_profile(&router, row.case, &profile, row.principal).await;
        let answered = emr_ok(
            &router,
            row.case,
            "RunJobFlow",
            &json!({
                "Name": format!("credentials-{ordinal}"),
                "ReleaseLabel": "emr-6.15.0",
                "JobFlowRole": profile,
                "ServiceRole": SERVICE_ROLE,
                "Applications": [{ "Name": "Hadoop" }],
                "Steps": [{
                    "Name": row.case,
                    "ActionOnFailure": "CONTINUE",
                    "HadoopJarStep": {
                        "Jar": "command-runner.jar",
                        "Args": command_line(row, ordinal),
                    },
                }],
                "Instances": {
                    "InstanceCount": 1,
                    "MasterInstanceType": "m5.xlarge",
                    "KeepJobFlowAliveWhenNoSteps": true,
                },
            }),
        )
        .await;
        let cluster_id = answered["JobFlowId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: RunJobFlow must answer a JobFlowId, got {answered}",
                    row.case
                )
            })
            .to_string();
        launched.push(cluster_id);
    }

    for ((ordinal, row), cluster_id) in rows.iter().enumerate().zip(&launched) {
        let case = row.case;
        let steps = drained(case, &router, cluster_id).await;
        let step = steps
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected the submitted step back"));
        let state = step["Status"]["State"].as_str().unwrap_or_default();
        let key = marker_key(ordinal);
        let held = object(&router, &key).await;
        if row.completes {
            assert_eq!(
                state, "COMPLETED",
                "case {case:?}: a cluster whose instances obtain a credential completes its \
                 step, got {step}"
            );
            assert_eq!(
                held.as_deref(),
                Some(expected_marker(row)),
                "case {case:?}: expected {key} to hold {:?}, got {held:?}",
                expected_marker(row)
            );
        } else {
            assert_eq!(
                state, "FAILED",
                "case {case:?}: a cluster whose instances obtain no credential fails the step \
                 whose work needs one, got {step}"
            );
            let reason = step["Status"]["FailureDetails"]["Reason"]
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                reason, COMMAND_FAILED_REASON,
                "case {case:?}: expected the command's own failure reason, got {step}"
            );
            assert_eq!(
                held, None,
                "case {case:?}: expected the store to hold nothing at {key}, got {held:?}"
            );
        }
    }
}
