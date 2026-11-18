// SPDX-License-Identifier: CC0-1.0

//! A headless Amazon EMR notebook execution runs its notebook on a router that VERIFIES
//! request signatures — the configuration the emulator serves traffic in.
//!
//! The RULE is the one the sibling suite
//! [`crate::emr_notebook_execution_runs_its_s3_notebook`] pins: **an execution's end is what
//! running its notebook answered — `FINISHED` with the executed notebook stored at the
//! `OutputNotebookS3Location` the request named for a notebook whose cells all ran, `FAILED`
//! with the engine's own exception for one that raised.**
//!
//! What this suite adds is the axis the sibling suite cannot cover: THAT one turns request
//! authentication OFF, so it cannot see whether the reads and writes Amazon EMR makes
//! THROUGH THE EMULATOR'S OWN CALLBACK — fetching the notebook out of Amazon S3, and storing
//! the executed notebook back into it — are accepted by a router that authenticates every
//! request, which is how the emulator is served. Both of those happen with no request of the
//! client's in flight: the notebook is run after `StartNotebookExecution` has already
//! answered. Every call below is signed with the keys this process publishes, and this
//! router verifies them.
//!
//! The rows span what the rule generalises over: what the notebook's cells come to (every
//! cell runs, a cell raises).
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on an explicitly sized thread.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use aws_credential_types::Credentials;
use aws_emulator_router::{build_router, InMemoryEngine};
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emr_notebook_execution_runs_its_s3_notebook::{
    notebook, notebook_key, trust_policy, BUCKET, DEADLINE, EC2_PRINCIPAL, INSTANCE_PROFILE,
    NOTEBOOK_ROLE, OUTPUT_PREFIX, POLL_INTERVAL, READY_STATES, REGION, SERVICE_ROLE,
    TERMINAL_STATUSES,
};

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The credential every call below signs with: the pair this process publishes once request
/// authentication is on.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    /// Sign `request` for `signing_name` in [`REGION`], in the `Authorization` header an SDK
    /// request carries.
    fn sign(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
    ) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }
}

/// One signed request, answered with the status and the body as bytes.
async fn signed(
    router: &MultiServiceRouter,
    signer: &Signer,
    signing_name: &str,
    built: http::Request<Vec<u8>>,
) -> (u16, Vec<u8>) {
    let signed = signer.sign(built, signing_name);
    let (parts, payload) = signed.into_parts();
    let request = http::Request::from_parts(parts, bytes::Bytes::from(payload));
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// One Amazon EMR call (`awsJson1_1`, `ElasticMapReduce.<Operation>`) that must be served.
async fn emr_ok(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let host = format!("elasticmapreduce.{REGION}.amazonaws.com");
    let built = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("ElasticMapReduce.{operation}"))
        .body(body.to_string().into_bytes())
        .expect("the test request is well formed");
    let (status, answered) = signed(router, signer, "elasticmapreduce", built).await;
    let parsed: Value = if answered.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answered).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One IAM `awsQuery` call, which must answer 200.
async fn iam(router: &MultiServiceRouter, signer: &Signer, case: &str, body: &str) {
    let built = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.as_bytes().to_vec())
        .expect("the test request is well formed");
    let (status, answered) = signed(router, signer, "iam", built).await;
    assert_eq!(
        status,
        200,
        "case {case:?}: expected HTTP 200 from IAM, got {status} with {}",
        String::from_utf8_lossy(&answered)
    );
}

/// One path-style Amazon S3 request: its status and body.
async fn s3(
    router: &MultiServiceRouter,
    signer: &Signer,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let built = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", &host)
        .body(body.to_vec())
        .expect("the test request is well formed");
    signed(router, signer, "s3", built).await
}

/// The object at `key`, or `None` when the store holds none.
async fn object(router: &MultiServiceRouter, signer: &Signer, key: &str) -> Option<String> {
    let (status, body) = s3(router, signer, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    (200..300)
        .contains(&status)
        .then(|| String::from_utf8_lossy(&body).to_string())
}

/// The bucket, the roles and the instance profile every row shares, each created through the
/// service that owns it.
async fn seed(router: &MultiServiceRouter, signer: &Signer) {
    let case = "setup";
    let (status, body) = s3(router, signer, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    for (role, principal) in [
        (SERVICE_ROLE, "elasticmapreduce.amazonaws.com"),
        (INSTANCE_PROFILE, EC2_PRINCIPAL),
    ] {
        iam(
            router,
            signer,
            case,
            &format!(
                "Action=CreateRole&Version=2010-05-08&RoleName={role}\
                 &AssumeRolePolicyDocument={}",
                trust_policy(principal)
            ),
        )
        .await;
    }
    iam(
        router,
        signer,
        case,
        &format!(
            "Action=CreateInstanceProfile&Version=2010-05-08\
             &InstanceProfileName={INSTANCE_PROFILE}&Path=/"
        ),
    )
    .await;
    iam(
        router,
        signer,
        case,
        &format!(
            "Action=AddRoleToInstanceProfile&Version=2010-05-08\
             &InstanceProfileName={INSTANCE_PROFILE}&RoleName={INSTANCE_PROFILE}"
        ),
    )
    .await;
}

/// The keep-alive cluster a headless notebook execution runs on, polled until it is up.
async fn notebook_cluster(router: &MultiServiceRouter, signer: &Signer) -> String {
    let case = "setup";
    let launched = emr_ok(
        router,
        signer,
        case,
        "RunJobFlow",
        &json!({
            "Name": "headless-notebooks",
            "ReleaseLabel": "emr-7.9.0",
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "Applications": [
                { "Name": "Hadoop" },
                { "Name": "Spark" },
                { "Name": "Livy" },
                { "Name": "JupyterEnterpriseGateway" },
            ],
            "Instances": {
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceGroups": [
                    {
                        "Name": "primary",
                        "InstanceRole": "MASTER",
                        "InstanceType": "m5.xlarge",
                        "InstanceCount": 1,
                    },
                ],
            },
        }),
    )
    .await;
    let cluster_id = launched["JobFlowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: RunJobFlow answers a JobFlowId, got {launched}"))
        .to_string();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let cluster = emr_ok(
            router,
            signer,
            case,
            "DescribeCluster",
            &json!({ "ClusterId": cluster_id.clone() }),
        )
        .await["Cluster"]
            .clone();
        let state = cluster["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if READY_STATES.contains(&state.as_str()) {
            return cluster_id;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the cluster never came up, last state {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `DescribeNotebookExecution` until the execution reaches a status it never leaves.
async fn settled(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    id: &str,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let execution = emr_ok(
            router,
            signer,
            case,
            "DescribeNotebookExecution",
            &json!({ "NotebookExecutionId": id }),
        )
        .await["NotebookExecution"]
            .clone();
        let status = execution["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if TERMINAL_STATUSES.contains(&status.as_str()) {
            return execution;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the execution never settled, last status {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row: the cells of the notebook this execution runs, the status it must end in, and a
/// part of the reason it must then carry.
struct Row {
    case: &'static str,
    cells: &'static [&'static str],
    ends_in: &'static str,
    reason: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a notebook whose cells all run",
            cells: &["print(1 + 1)"],
            ends_in: "FINISHED",
            reason: None,
        },
        Row {
            case: "a notebook whose cell raises",
            cells: &["raise ValueError('the notebook failed')"],
            ends_in: "FAILED",
            reason: Some("ValueError"),
        },
    ]
}

/// Run `body` on a thread with [`BOOT_STACK_SIZE`]: assembling every service and dispatching
/// a chain of in-process calls through it nests more future than a test thread's own stack
/// holds.
fn driven<Body>(body: Body)
where
    Body: FnOnce() -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ()>>> + Send + 'static,
{
    ::std::thread::Builder::new()
        .name("emr-notebook-authenticating".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let runtime = ::tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .expect("build a runtime");
            runtime.block_on(body());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

#[test]
fn a_notebook_execution_runs_and_stores_its_output_on_an_authenticating_router() {
    driven(|| Box::pin(execution_runs_on_an_authenticating_router()));
}

async fn execution_runs_on_an_authenticating_router() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    // Every call runs with request authentication ON, as the emulator serves traffic — and
    // so do the reads and writes Amazon EMR itself makes through the emulator's callback.
    router.set_request_authentication(true);
    let published = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning authentication on publishes this process's keys");
    let signer = Signer {
        key_id: published.access_key_id.clone(),
        secret: published.secret_access_key.clone(),
    };

    seed(&router, &signer).await;
    let cluster_id = notebook_cluster(&router, &signer).await;

    let rows = rows();
    let mut started: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let key = notebook_key(ordinal);
        let (status, answered) = s3(
            &router,
            &signer,
            "PUT",
            &format!("/{BUCKET}/{key}"),
            &notebook(row.cells),
        )
        .await;
        assert!(
            (200..300).contains(&status),
            "case {:?}: PutObject {key} must succeed, got {status} {}",
            row.case,
            String::from_utf8_lossy(&answered)
        );
        let answer = emr_ok(
            &router,
            &signer,
            row.case,
            "StartNotebookExecution",
            &json!({
                "ExecutionEngine": { "Id": cluster_id.clone(), "Type": "EMR" },
                "ServiceRole": NOTEBOOK_ROLE,
                "NotebookExecutionName": row.case,
                "NotebookS3Location": { "Bucket": BUCKET, "Key": key },
                "OutputNotebookS3Location": { "Bucket": BUCKET, "Key": OUTPUT_PREFIX },
            }),
        )
        .await;
        started.push(
            answer["NotebookExecutionId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: StartNotebookExecution answers an id, got {answer}",
                        row.case
                    )
                })
                .to_string(),
        );
    }

    for (row, id) in rows.iter().zip(&started) {
        let case = row.case;
        let execution = settled(&router, &signer, case, id).await;
        let reason = execution["LastStateChangeReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            execution["Status"].as_str(),
            Some(row.ends_in),
            "case {case:?}: expected {} on an authenticating router, got {execution}",
            row.ends_in
        );
        match row.reason {
            Some(expected) => assert!(
                reason.contains(expected),
                "case {case:?}: expected the engine's own reason to carry {expected:?}, got \
                 {reason:?}"
            ),
            None => {
                assert!(
                    reason.is_empty(),
                    "case {case:?}: an execution that completed without error reports no \
                     reason, got {reason:?}"
                );
                // The write Amazon EMR made through the callback was accepted too: the
                // output notebook is an object the store really holds.
                let uri = execution["OutputNotebookURI"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: a finished execution reports its output file, got \
                             {execution}"
                        )
                    })
                    .to_string();
                let key = uri
                    .strip_prefix(&format!("s3://{BUCKET}/"))
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: the output file is an object of {BUCKET}, got {uri:?}"
                        )
                    });
                let stored = object(&router, &signer, key).await;
                assert!(
                    stored.is_some_and(|document| document.contains("execution_count")),
                    "case {case:?}: the store holds the executed notebook at {key:?}"
                );
            }
        }
    }
}
