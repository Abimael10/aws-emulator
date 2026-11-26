// SPDX-License-Identifier: CC0-1.0

//! An accepted Bedrock Runtime asynchronous invocation reaches a TERMINAL status on the
//! ASSEMBLED emulator, and its output is in the caller's Amazon S3 bucket by then.
//!
//! Every client of the asynchronous surface is a poll loop: `StartAsyncInvoke` mints an
//! `invocationArn` and the caller reads `GetAsyncInvoke` until the invocation leaves the
//! one non-terminal value the modelled `AsyncInvokeStatus` enum carries
//! (`InProgress | Completed | Failed`). So an accepted invocation has to converge to a
//! terminal value, and the success path has to leave the artifact under the configured
//! `outputDataConfig.s3OutputDataConfig.s3Uri` before it reports `Completed` — otherwise a
//! caller that waits for it never gets an output location it can read.
//!
//! This suite drives that across the two services it needs at once — Bedrock Runtime and
//! Amazon S3 — on the emulator as `aws_emulator_router::build_router` assembles it, so the
//! bucket a caller creates through Amazon S3 is the bucket the invocation's output is read
//! back out of.
//!
//! The owning crate's own
//! `service-com-amazonaws-bedrockruntime-amazonbedrockfrontendservice/tests/all/async_invoke_output_artifact.rs`
//! reads the ARTIFACT in depth — the geometry and the length the submitted `modelInput`
//! configured, decoded by the MP4 reader — over a two-service router. What is added here is
//! the ASSEMBLED emulator: every service mounted, as the binary serves it, which is what
//! also rules out another service claiming `POST /async-invoke` or
//! `GET /async-invoke/{invocationArn}` out from under this one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// How long a poll waits for an invocation to leave `InProgress`.
///
/// Nothing in the emulator sleeps for the transition, so the test looks again until it
/// holds; the budget is far above the span the transition is due after, which is what keeps
/// a loaded or faster machine from changing the outcome.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll waits between two looks. It bounds nothing the test asserts.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One answer: status and body.
struct Call {
    status: u16,
    body: Bytes,
}

impl Call {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is
/// what routes the request to the service that claims it. The signature is never checked:
/// the router here runs with request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTBEDROCKRUNTIME/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000000000000000000000000000000000000000\
         000000000000000000000000"
    )
}

/// The emulator as the binary serves it: every service on one engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    router.set_request_authentication(false);
    router
}

/// One request on the assembled router, signed for `signing_name` and addressed to that
/// service's own endpoint host.
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    endpoint: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Call {
    let host = format!("{endpoint}.{REGION}.amazonaws.com");
    let payload = body.map(Value::to_string).unwrap_or_default();
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name));
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Call {
        status: parts.status.as_u16(),
        body,
    }
}

/// One Amazon S3 request, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str) -> Call {
    call(router, "s3", "s3", method, path, None).await
}

/// One Bedrock Runtime request.
async fn runtime(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Call {
    call(router, "bedrock", "bedrock-runtime", method, path, body).await
}

/// Percent-encode what an ARN carries that cannot sit in a path segment unescaped, which
/// is how an SDK puts an `invocationArn` into `GET /async-invoke/{invocationArn}`.
fn encode_path(value: &str) -> String {
    value.replace(':', "%3A").replace('/', "%2F")
}

/// The keys Amazon S3 reports under `prefix`, and the size it reports for each, read
/// through its own `ListObjectsV2` — the call a client collecting the output makes.
async fn listed_objects(
    router: &MultiServiceRouter,
    bucket: &str,
    prefix: &str,
) -> Vec<(String, u64)> {
    let listed = s3(
        router,
        "GET",
        &format!("/{bucket}?list-type=2&prefix={prefix}"),
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "listing {prefix:?} must answer 200, got {} / {}",
        listed.status,
        listed.text()
    );
    let body = listed.text();
    // The listing is read off the document Amazon S3's own serializer writes, so the
    // document itself is checked before its entries are: a body of another shape would
    // otherwise scrape as "no objects" instead of naming what was answered.
    assert!(
        body.contains("<ListBucketResult"),
        "listing {prefix:?} must answer a ListObjectsV2 result, got {body}"
    );
    body.split("<Contents>")
        .skip(1)
        .filter_map(|entry| {
            let key = entry.split("<Key>").nth(1)?.split("</Key>").next()?;
            let size = entry
                .split("<Size>")
                .nth(1)
                .and_then(|tail| tail.split("</Size>").next())
                .and_then(|size| size.trim().parse::<u64>().ok())
                .unwrap_or_default();
            Some((key.to_string(), size))
        })
        .collect()
}

/// Read the invocation `arn` names until it leaves the one non-terminal status, and report
/// the record it settled into.
async fn poll_to_terminal(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let read = runtime(
            router,
            "GET",
            &format!("/async-invoke/{}", encode_path(arn)),
            None,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected 200 from GetAsyncInvoke, got {} / {}",
            read.status,
            read.text()
        );
        let record = read.json();
        if record["status"].as_str() != Some("InProgress") {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the invocation never reached a terminal status, last read {record}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One invocation: the model it names, the `modelInput` that model's published request
/// schema takes, and the output location it configures.
struct Row {
    case: &'static str,
    bucket: &'static str,
    model_id: &'static str,
    model_input: Value,
    /// The configured `s3Uri`.
    s3_uri: String,
    /// The key prefix of that `s3Uri`, empty for a location addressing the bucket root.
    prefix: &'static str,
}

/// The rows, differing along the invoked model, along which published members the payload
/// carries, and along the SHAPE of the configured location (a bucket root written with a
/// trailing separator, a one-segment prefix, a deep prefix).
///
/// Members and values from the request tables in the Amazon Bedrock User Guide: "Luma AI
/// Ray v2" (`prompt`, `aspect_ratio`, `loop`, `duration`, `resolution`) and "Amazon Nova
/// Reel" (`taskType`, `textToVideoParams.text`, `videoGenerationConfig.durationSeconds` /
/// `fps` / `dimension`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a Luma generation into the bucket root",
            bucket: "async-invoke-root",
            model_id: "luma.ray-v2:0",
            model_input: json!({
                "prompt": "Closeup of a cute old steampunk robot. Camera zoom in.",
                "aspect_ratio": "16:9",
                "loop": false,
                "duration": "5s",
                "resolution": "540p",
            }),
            s3_uri: "s3://async-invoke-root/".to_string(),
            prefix: "",
        },
        Row {
            case: "a Nova Reel generation under one prefix segment",
            bucket: "async-invoke-prefixed",
            model_id: "amazon.nova-reel-v1:0",
            model_input: json!({
                "taskType": "TEXT_VIDEO",
                "textToVideoParams": {"text": "A kite over a lake at dawn."},
                "videoGenerationConfig": {
                    "durationSeconds": 6,
                    "fps": 24,
                    "dimension": "1280x720",
                },
            }),
            s3_uri: "s3://async-invoke-prefixed/renders".to_string(),
            prefix: "renders",
        },
        Row {
            case: "a Luma generation under a deep prefix",
            bucket: "async-invoke-deep",
            model_id: "luma.ray-v2:0",
            model_input: json!({"prompt": "A lantern rising over a field."}),
            s3_uri: "s3://async-invoke-deep/renders/2026/january/".to_string(),
            prefix: "renders/2026/january",
        },
    ]
}

/// Create the row's output bucket through Amazon S3 and submit its invocation, reporting
/// the `invocationArn` the service minted — the two calls a client makes before it starts
/// polling.
async fn start_invocation(router: &MultiServiceRouter, row: &Row) -> String {
    let case = row.case;
    let created = s3(router, "PUT", &format!("/{}", row.bucket)).await;
    assert!(
        (200..300).contains(&created.status),
        "case {case:?}: the output bucket must be created, got {} / {}",
        created.status,
        created.text()
    );
    let submitted = runtime(
        router,
        "POST",
        "/async-invoke",
        Some(&json!({
            "modelId": row.model_id,
            "modelInput": row.model_input,
            "outputDataConfig": {"s3OutputDataConfig": {"s3Uri": row.s3_uri}},
        })),
    )
    .await;
    assert_eq!(
        submitted.status,
        200,
        "case {case:?}: expected StartAsyncInvoke to accept the invocation, got {} / {}",
        submitted.status,
        submitted.text()
    );
    submitted.json()["invocationArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no invocationArn in {}", submitted.text()))
        .to_string()
}

/// RULE: an accepted asynchronous invocation converges to a terminal `AsyncInvokeStatus`,
/// and on the success path it has written its output under the configured location before
/// any read reports it `Completed` — which is what a polling client is waiting for.
#[tokio::test]
async fn an_accepted_async_invocation_reaches_completed_with_its_output_in_the_bucket() {
    let router = emulator();
    for row in rows() {
        let case = row.case;
        let arn = start_invocation(&router, &row).await;

        let record = poll_to_terminal(&router, case, &arn).await;
        assert_eq!(
            record["status"].as_str(),
            Some("Completed"),
            "case {case:?}: expected the invocation to complete, got {record}"
        );
        // `submitTime` is required and a terminal record states when it ended, so both are
        // read as the numbers they are rather than compared as absent values.
        let submit_time = record["submitTime"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: every record states when it was submitted, got {record}")
        });
        let end_time = record["endTime"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: a terminal invocation states when it ended, got {record}")
        });
        assert!(
            end_time >= submit_time,
            "case {case:?}: the invocation cannot end before it was submitted, got {record}"
        );
        assert_eq!(
            record["lastModifiedTime"].as_f64(),
            Some(end_time),
            "case {case:?}: the terminal transition is the record's last change, got {record}"
        );
        assert!(
            record["failureMessage"].is_null(),
            "case {case:?}: a completed invocation carries no failure message, got {record}"
        );
        // The location the record reports is this invocation's OWN: the location the
        // submission configured, extended by the minted invocation id as one further key
        // segment, which is where the output is.
        let invocation_id = arn.rsplit('/').next().unwrap_or_default();
        let reported = format!("{}/{invocation_id}", row.s3_uri.trim_end_matches('/'));
        assert_eq!(
            record["outputDataConfig"]["s3OutputDataConfig"]["s3Uri"].as_str(),
            Some(reported.as_str()),
            "case {case:?}: expected the configured location extended by the minted \
             invocation id, got {record}"
        );

        // What the client then reads: a non-empty object under the key prefix of the
        // location the RECORD reports, listed through Amazon S3's own operation — so what
        // the service says it wrote is where a client collecting the output looks.
        let segment = if row.prefix.is_empty() {
            invocation_id.to_string()
        } else {
            format!("{}/{invocation_id}", row.prefix)
        };
        let objects = listed_objects(&router, row.bucket, &segment).await;
        assert!(
            objects.iter().any(|(_, size)| *size > 0),
            "case {case:?}: expected a non-empty object under {segment:?}, got {objects:?}"
        );
    }
}

/// RULE: a terminal invocation keeps answering the same record, and its artifact stays
/// where it was written — reading it changes nothing.
#[tokio::test]
async fn a_completed_invocation_and_its_output_stay_as_they_settled() {
    let router = emulator();
    let row = &rows()[0];
    let case = row.case;
    let arn = start_invocation(&router, row).await;
    let settled = poll_to_terminal(&router, case, &arn).await;
    assert_eq!(
        settled["status"].as_str(),
        Some("Completed"),
        "case {case:?}: expected the invocation to complete, got {settled}"
    );
    // The artifact the stability loop below comes back for: there is something to come
    // back FOR, so the loop cannot pass by comparing one empty listing against another.
    let first = listed_objects(&router, row.bucket, row.prefix).await;
    assert!(
        first.iter().any(|(_, size)| *size > 0),
        "case {case:?}: expected a non-empty object under {:?}, got {first:?}",
        row.prefix
    );

    for look in 1..=5 {
        let again = poll_to_terminal(&router, case, &arn).await;
        assert_eq!(
            again, settled,
            "case {case:?} look {look}: expected the identical record, got {again}"
        );
        let objects = listed_objects(&router, row.bucket, row.prefix).await;
        assert_eq!(
            objects, first,
            "case {case:?} look {look}: expected the identical objects, got {objects:?}"
        );
    }
}
