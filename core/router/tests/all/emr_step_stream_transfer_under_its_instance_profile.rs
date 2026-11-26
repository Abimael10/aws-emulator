// SPDX-License-Identifier: CC0-1.0

//! A step's command line carries a file to and from Amazon S3 over the CLI's own STREAM
//! operand, on a transient cluster that shuts itself down once its queue has run.
//!
//! The RULE under test: **a `command-runner.jar` step's arguments are a command line, so every
//! stage of that line runs and the step's terminal `StepState` is the status the line exited
//! with — `COMPLETED` for zero and `FAILED` otherwise (`AddJobFlowSteps`: "for a step to be
//! considered complete, the main function must exit with a zero exit code") — whatever form
//! the line's Amazon S3 transfers take and whether the cluster stays up after the queue or
//! shuts down; and the transfer is made as the role the cluster's instances obtain from their
//! instance profile, so a profile whose role admits Amazon EC2 writes the object and one whose
//! role trusts another service writes nothing and fails the step.**
//!
//! Only a cross-service suite can pin this: IAM holds the instance profiles and the roles
//! whose trust decides the credential, Amazon EMR runs the step, and the objects the command
//! line carries are real objects of Amazon S3's own store — read back here through `GetObject`,
//! never through anything this suite handed Amazon EMR. It therefore runs against the whole
//! `build_router` service set, as the binary serves it.
//!
//! The rows differ along what the rule generalises over: WHICH end of the transfer is the
//! command's own stream (an upload from it, a download to it, and a line that is nothing but a
//! file transfer, so nothing is keyed to one form) and WHICH service principal the
//! instance-profile role trusts. Every role here carries the cluster role's own managed
//! policy, so no row turns on what an identity policy allows: what a role's PERMISSIONS let a
//! step's work do is a rule of its own, separate from the credential its trust policy decides,
//! and this suite reads only the second.
//!
//! Every cluster here is TRANSIENT — it shuts itself down once its queue has run — and each
//! row's step is read back after that shutdown, which is what this suite adds over its
//! siblings: what a profile earns at the API is `emr_job_flow_role_instance_profile.rs`'s rule,
//! and the same credential rule over whole-FILE transfers on a cluster that STAYS UP is
//! `emr_step_instance_profile_credentials.rs`'s. The one row here whose line transfers a file
//! rather than a stream is kept for the same reason the principals vary: it is what shows the
//! terminal state follows the command's status and not the form its transfer took.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: the one whose `CreateBucket` needs no
/// location constraint in its body.
const REGION: &str = "us-east-1";

/// The bucket every step of this suite reads from and writes to.
const BUCKET: &str = "emr-step-stream-transfers";

/// The object the rows that READ one read, and the bytes it holds.
const SEED_KEY: &str = "in/seed.txt";
const SEED_BYTES: &str = "seeded\n";

/// What a step's command line echoes into the object it writes, which is what that object then
/// holds.
const ECHOED_BYTES: &str = "ok\n";

/// The service role every launch names — "the IAM role that Amazon EMR assumes in order to
/// access Amazon Web Services resources on your behalf" (`RunJobFlow$ServiceRole`).
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// The service principal an Amazon EC2 instance assumes a role as, which is what a role an
/// instance profile carries has to trust for the instance to obtain a credential for it.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The AWS managed policy an Amazon EMR cluster's instance-profile role carries, named here by
/// its published ARN (`arn:aws:iam::aws:policy/service-role/<name>`, the form
/// `AttachRolePolicy` takes).
const EC2_ROLE_POLICY_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonElasticMapReduceforEC2Role";

/// `FailureDetails$Reason` of a step whose command exited with a non-zero status, spelled out
/// here rather than imported from the service crate, so a wrong constant in the implementation
/// cannot make this suite pass.
const COMMAND_FAILED_REASON: &str = "The command the step submitted exited with a non-zero status.";

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
const TERMINAL_STEP_STATES: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// The states a cluster never leaves (`com.amazonaws.emr#ClusterState`).
const TERMINAL_CLUSTER_STATES: [&str; 3] =
    ["TERMINATED", "TERMINATED_WITH_ERRORS", "TERMINATING_FAILED"];

/// How long a poll waits for a cluster to settle: both provisioning phases, every step's own
/// span, one command's transfers and the shutdown, with room to spare. The wait is on the
/// CONDITION — it looks again until the cluster has settled — so a loaded host delays a row
/// instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(240);

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
async fn emr(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
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

/// What `GetObject` answers for `key`: its HTTP status and, for an object the store holds, the
/// text it holds. The status is answered beside the bytes so that an absence is read as the
/// store's own 404 rather than as any answer that is not a success.
async fn object(router: &MultiServiceRouter, key: &str) -> (u16, Option<String>) {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    let held = (200..300)
        .contains(&status)
        .then(|| String::from_utf8_lossy(&body).to_string());
    (status, held)
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

/// An instance profile called `name` carrying a role of the same name whose trust policy admits
/// `principal` and which carries the cluster role's own managed policy, built the way AWS
/// builds one: `CreateRole`, `AttachRolePolicy`, `CreateInstanceProfile`,
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
            "Action=AttachRolePolicy&Version=2010-05-08&RoleName={name}&PolicyArn={}",
            EC2_ROLE_POLICY_ARN.replace(':', "%3A").replace('/', "%2F")
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

/// WHICH end of the step's Amazon S3 transfer is the command's own stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Work {
    /// A pipeline whose first stage writes the bytes and whose second uploads them from the
    /// command's own standard input: `aws s3 cp - s3://<bucket>/<key>`.
    UploadFromStream,
    /// A pipeline whose first stage downloads the seeded object onto its standard output and
    /// whose second uploads what it read: `aws s3 cp s3://<bucket>/<seed> - | aws s3 cp - …`.
    DownloadToStreamThenUpload,
    /// Neither end is a stream: a file is written and then uploaded.
    UploadAFile,
}

/// One row: the service principal the cluster's instance-profile role trusts, the work its one
/// step submits, and whether that step completes.
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
            case: "an Amazon EC2 profile, uploading from the command's own stream",
            principal: EC2_PRINCIPAL,
            work: Work::UploadFromStream,
            completes: true,
        },
        Row {
            case: "an Amazon EC2 profile, downloading onto the command's own stream and \
                   uploading what it read",
            principal: EC2_PRINCIPAL,
            work: Work::DownloadToStreamThenUpload,
            completes: true,
        },
        Row {
            case: "an Amazon EC2 profile, uploading a file",
            principal: EC2_PRINCIPAL,
            work: Work::UploadAFile,
            completes: true,
        },
        Row {
            case: "an AWS Glue profile, uploading from the command's own stream",
            principal: "glue.amazonaws.com",
            work: Work::UploadFromStream,
            completes: false,
        },
        Row {
            case: "an AWS Lambda profile, downloading onto the command's own stream and \
                   uploading what it read",
            principal: "lambda.amazonaws.com",
            work: Work::DownloadToStreamThenUpload,
            completes: false,
        },
    ]
}

/// The key one row's step writes.
fn marker_key(ordinal: usize) -> String {
    format!("proof/marker-{ordinal}.txt")
}

/// The command line one row's step submits, as a shell program after `-c` — the form the CLI's
/// `--steps` writes.
fn command_line(row: &Row, ordinal: usize) -> Vec<String> {
    let marker = marker_key(ordinal);
    let program = match row.work {
        Work::UploadFromStream => {
            format!("echo ok | aws s3 cp - s3://{BUCKET}/{marker}")
        }
        Work::DownloadToStreamThenUpload => format!(
            "aws s3 cp s3://{BUCKET}/{SEED_KEY} - | aws s3 cp - s3://{BUCKET}/{marker}"
        ),
        Work::UploadAFile => format!(
            "echo ok > /tmp/marker.txt && aws s3 cp /tmp/marker.txt s3://{BUCKET}/{marker}"
        ),
    };
    vec!["bash".to_string(), "-c".to_string(), program]
}

/// What one row's marker must hold once its step has completed.
fn expected_marker(row: &Row) -> &'static str {
    match row.work {
        Work::UploadFromStream | Work::UploadAFile => ECHOED_BYTES,
        Work::DownloadToStreamThenUpload => SEED_BYTES,
    }
}

/// Poll `DescribeCluster` until `cluster_id` has settled, the way a client waits for a
/// transient cluster to shut itself down, and answer its terminal state.
async fn settled_cluster(case: &str, router: &MultiServiceRouter, cluster_id: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = emr_ok(
            router,
            case,
            "DescribeCluster",
            &json!({ "ClusterId": cluster_id }),
        )
        .await;
        let state = described["Cluster"]["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if TERMINAL_CLUSTER_STATES.contains(&state.as_str()) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the transient cluster to shut itself down, got {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The steps of `cluster_id` in submission order, each already terminal.
async fn steps(case: &str, router: &MultiServiceRouter, cluster_id: &str) -> Vec<Value> {
    let listed = emr_ok(router, case, "ListSteps", &json!({ "ClusterId": cluster_id })).await;
    let mut listed: Vec<Value> = listed["Steps"].as_array().cloned().unwrap_or_default();
    listed.reverse();
    let states: Vec<String> = listed
        .iter()
        .map(|step| {
            step["Status"]["State"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert!(
        !states.is_empty()
            && states
                .iter()
                .all(|state| TERMINAL_STEP_STATES.contains(&state.as_str())),
        "case {case:?}: a cluster that has shut down has run its whole queue, got {states:?}"
    );
    listed
}

#[tokio::test]
async fn a_steps_stream_transfers_are_made_as_the_role_its_instance_profile_carries() {
    let router = router();
    let rows = rows();

    seed_store(&router).await;
    create_service_role(&router, "setup").await;

    // One profile and one cluster per row, so each row's step is answered on a cluster of its
    // own. Every launch is accepted — a trust mismatch is no API-time refusal — and the
    // clusters are launched before any of them is polled, so their spans run together.
    let mut launched: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let profile = format!("EMR_EC2_Stream_Profile_{ordinal}");
        create_instance_profile(&router, row.case, &profile, row.principal).await;
        let answered = emr_ok(
            &router,
            row.case,
            "RunJobFlow",
            &json!({
                "Name": format!("stream-transfer-{ordinal}"),
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
                    "KeepJobFlowAliveWhenNoSteps": false,
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
        // A queue every step of which ran is the shutdown `ALL_STEPS_COMPLETED` brings, and a
        // step that failed with `ActionOnFailure` `CONTINUE` is no failure OF THE CLUSTER, so
        // every row's cluster ends `TERMINATED` rather than `TERMINATED_WITH_ERRORS`.
        let settled = settled_cluster(case, &router, cluster_id).await;
        assert_eq!(
            settled, "TERMINATED",
            "case {case:?}: a transient cluster that ran its queue ends TERMINATED, got \
             {settled:?}"
        );
        let listed = steps(case, &router, cluster_id).await;
        let step = listed
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected the submitted step back"));
        let state = step["Status"]["State"].as_str().unwrap_or_default();
        let key = marker_key(ordinal);
        let (status, held) = object(&router, &key).await;
        if row.completes {
            assert_eq!(
                state, "COMPLETED",
                "case {case:?}: a command line that exits zero completes its step, got {step}"
            );
            assert_eq!(
                held.as_deref(),
                Some(expected_marker(row)),
                "case {case:?}: expected {key} to hold {:?}, got {status} with {held:?}",
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
            // The store's own answer for a key it holds nothing under, so an object that is
            // absent is told apart from a read that failed for another reason.
            assert_eq!(
                (status, held.as_deref()),
                (404, None),
                "case {case:?}: expected {key} to be absent, got {status} with {held:?}"
            );
        }
    }
}
