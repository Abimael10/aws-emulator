// SPDX-License-Identifier: CC0-1.0

//! Model invocation logging delivers on the emulator as it is ASSEMBLED and SERVED: every
//! service mounted on one engine, request signatures verified, and the destination bucket
//! read back the way a stock client reads it.
//!
//! RULE: while a model-invocation-logging configuration naming an object destination is in
//! force, every inference the Bedrock Runtime data plane serves delivers one record under
//! that destination's own `keyPrefix`, named by the id the caller was answered under; the
//! delete of the configuration stops the records, and a re-install resumes them for the
//! invocations served after it without flushing the ones served while none was installed.
//!
//! The sibling suites drive three services on a router that verifies nothing and addresses
//! the model id as written; this one drives the WHOLE service set
//! (`aws_emulator_router::build_router`) with request authentication ON, signs every call
//! the way an SDK signs it, addresses the model id percent-encoded the way an SDK puts a URI
//! label on the wire, and holds the destination bucket in a Region of its own with the
//! Bedrock log-write bucket policy attached — which is the configuration a client meets.
//!
//! The rows span what the rule generalises over: the inference surface that served the call
//! and the prefix the destination is configured with.

use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state::invocation_logging as logging;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for and addressed in — not the one Amazon S3 places
/// an unconstrained bucket in, so the bucket is created with an explicit constraint.
const REGION: &str = "us-west-2";

/// The bucket every destination below delivers into.
const BUCKET: &str = "bedrock-assembled-invocation-logs";

/// The model every inference below is served on.
const MODEL: &str = "amazon.nova-lite-v1:0";

/// The same model id as an SDK puts it on the wire: a URI label with its `:` escaped.
const MODEL_LABEL: &str = "amazon.nova-lite-v1%3A0";

/// The URI the three model-invocation-logging operations are bound to.
const LOGGING_URI: &str = "/logging/modelinvocations";

/// The header this protocol reports the request a response answers in.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The XML namespace a `CreateBucketConfiguration` carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// How long a case waits for a delivered record before concluding nothing was delivered.
/// Delivery is part of serving the inference, so this is room for a loaded machine rather
/// than a span anything is expected to take.
const DELIVERY_BUDGET: Duration = Duration::from_secs(30);

/// How long a case waits between two listings of the destination.
const POLL_STEP: Duration = Duration::from_millis(50);

/// The account the emulator serves, which every delivered key names.
fn account_id() -> String {
    ::engine::emulator_account_id().to_string()
}

// ---------------------------------------------------------------------------------------
// The harness: the assembled emulator, authenticating, signed the way an SDK signs
// ---------------------------------------------------------------------------------------

fn emulator() -> MultiServiceRouter {
    let engine = ::aws_emulator_router::new_engine();
    let mut router = ::aws_emulator_router::build_router(engine);
    router.set_request_authentication(true);
    router
}

/// The credential every call below signs with: the pair this process publishes once request
/// authentication is on.
fn published() -> &'static (String, String) {
    static PUBLISHED: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    PUBLISHED.get_or_init(|| {
        let minted = ::emulator_server::SigV4Authenticator::minted_credentials()
            .expect("turning request authentication on publishes this process's keys");
        (
            minted.access_key_id.clone(),
            minted.secret_access_key.clone(),
        )
    })
}

/// `request`, signed for `signing_name` in [`REGION`] the way an SDK signs one.
fn signed(mut request: http::Request<Vec<u8>>, signing_name: &str) -> http::Request<Bytes> {
    let (key_id, secret) = published();
    let identity = Credentials::new(key_id, secret, None, None, "test").into();
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
    let (parts, payload) = request.into_parts();
    http::Request::from_parts(parts, Bytes::from(payload))
}

/// One answered call: its status, the id it named the request by, and its body.
struct Answer {
    status: u16,
    request_id: String,
    body: Bytes,
}

impl Answer {
    /// The body as JSON, or `Null` for a body that is not a JSON document.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The body as text, for an XML answer and for a refusal quoted in an assertion.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// One signed request to the assembled emulator.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Answer {
    let host = format!("{host}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", content_type)
        .body(body)
        .expect("a well-formed request");
    let response = router.dispatch(signed(request, signing_name)).await;
    let status = response.status().as_u16();
    let request_id = response
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Answer {
        status,
        request_id,
        body: response.into_body(),
    }
}

/// One restJson1 call to the Bedrock control plane's logging configuration.
async fn bedrock(router: &MultiServiceRouter, method: &str, body: Option<Value>) -> Answer {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("the test body serializes"))
        .unwrap_or_default();
    send(
        router,
        "bedrock",
        "bedrock",
        method,
        LOGGING_URI,
        "application/json",
        payload,
    )
    .await
}

/// How a client addresses the destination bucket: in the path, or in the HOST, which is how
/// an AWS SDK addresses a bucket by default.
#[derive(Clone, Copy, Debug)]
enum Addressing {
    Path,
    VirtualHosted,
}

/// One signed Amazon S3 request against the destination bucket, addressed the way `style`
/// says: `suffix` is empty for the bucket itself and `/<key>` for one of its objects, and
/// `query` carries the query string as a client puts it on the wire.
async fn s3(
    router: &MultiServiceRouter,
    style: Addressing,
    method: &str,
    suffix: &str,
    query: &str,
    body: &[u8],
) -> Answer {
    let (host, path) = match style {
        Addressing::Path => ("s3".to_string(), format!("/{BUCKET}{suffix}{query}")),
        Addressing::VirtualHosted => {
            let addressed = if suffix.is_empty() { "/" } else { suffix };
            (format!("{BUCKET}.s3"), format!("{addressed}{query}"))
        }
    };
    send(
        router,
        "s3",
        &host,
        method,
        &path,
        "application/xml",
        body.to_vec(),
    )
    .await
}

/// The destination bucket, created in this suite's own Region and carrying the bucket policy
/// that lets Amazon Bedrock write the records into it.
async fn given_bucket(router: &MultiServiceRouter, case: &str, style: Addressing) {
    let configuration = format!(
        "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
         <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
    );
    let created = s3(
        router,
        style,
        "PUT",
        "",
        "",
        configuration.as_bytes(),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: the destination bucket must be created, got {} with {:?}",
        created.status,
        created.text()
    );
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "bedrock.amazonaws.com" },
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
        }],
    });
    let attached = s3(
        router,
        style,
        "PUT",
        "",
        "?policy",
        serde_json::to_vec(&policy)
            .expect("the policy serializes")
            .as_slice(),
    )
    .await;
    assert_eq!(
        attached.status,
        204,
        "case {case:?}: the log-write bucket policy must be attached, got {} with {:?}",
        attached.status,
        attached.text()
    );
}

/// Every key the destination bucket holds under `prefix`, as `ListObjectsV2` reports them.
/// `encoding-type=url` travels with the listing exactly as an AWS SDK sends it.
async fn delivered_keys(
    router: &MultiServiceRouter,
    case: &str,
    style: Addressing,
    prefix: &str,
) -> Vec<String> {
    let listed = s3(
        router,
        style,
        "GET",
        "",
        &format!("?list-type=2&prefix={prefix}&encoding-type=url"),
        &[],
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "case {case:?}: the destination bucket must be listable, got {} with {:?}",
        listed.status,
        listed.text()
    );
    listed
        .text()
        .split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split("</Key>").next())
        .map(str::to_string)
        .collect()
}

/// The records one delivered object carries: fetched through `GetObject` and read back in the
/// form the delivery wrote them.
async fn records_of(
    router: &MultiServiceRouter,
    case: &str,
    style: Addressing,
    key: &str,
) -> Vec<Value> {
    let fetched = s3(router, style, "GET", &format!("/{key}"), "", &[]).await;
    assert_eq!(
        fetched.status, 200,
        "case {case:?}: the delivered object {key:?} must be readable, got {}",
        fetched.status
    );
    logging::delivered_records(&fetched.body).unwrap_or_else(|| {
        panic!(
            "case {case:?}: the delivered object {key:?} is not a vended-log object, got {} \
             byte(s)",
            fetched.body.len()
        )
    })
}

/// Every record the destination holds under `prefix`, in the order the keys are listed.
async fn delivered_records(
    router: &MultiServiceRouter,
    case: &str,
    style: Addressing,
    prefix: &str,
) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    for key in delivered_keys(router, case, style, prefix).await {
        for record in records_of(router, case, style, &key).await {
            found.push((key.clone(), record));
        }
    }
    found
}

/// Poll the destination until it holds a record naming `request_id`, as a consumer holding a
/// response polls for the record of that very call.
async fn awaited_record(
    router: &MultiServiceRouter,
    case: &str,
    style: Addressing,
    prefix: &str,
    request_id: &str,
) -> (String, Value) {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let found = delivered_records(router, case, style, prefix).await;
        if let Some(record) = found
            .iter()
            .find(|(_, record)| record["requestId"].as_str() == Some(request_id))
        {
            return record.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no record naming the answered request {request_id:?} appeared \
             under {prefix:?}; {} record(s) present, requestIds {:?}",
            found.len(),
            found
                .iter()
                .map(|(_, record)| record["requestId"].clone())
                .collect::<Vec<_>>()
        );
        ::tokio::time::sleep(POLL_STEP).await;
    }
}

// ---------------------------------------------------------------------------------------
// The surfaces and the configurations
// ---------------------------------------------------------------------------------------

/// One row: the inference surface a call is served on, the destination prefix the
/// configuration in force names, and how the reader addresses the destination bucket.
struct Row {
    case: &'static str,
    /// The last segment of the inference URI, under `/model/<modelId>/`.
    path: &'static str,
    /// The `operation` the record of a call on this surface states.
    operation: &'static str,
    /// `s3Config.keyPrefix`, or `None` for a configuration that names none.
    key_prefix: Option<&'static str>,
    /// How the bucket is addressed by the client that holds and reads it.
    style: Addressing,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "Converse under a one-segment prefix, bucket addressed in the host",
            path: "converse",
            operation: logging::OPERATION_CONVERSE,
            key_prefix: Some("sim2real"),
            style: Addressing::VirtualHosted,
        },
        Row {
            case: "InvokeModel under a nested prefix, bucket addressed in the path",
            path: "invoke",
            operation: logging::OPERATION_INVOKE_MODEL,
            key_prefix: Some("audit/bedrock"),
            style: Addressing::Path,
        },
        Row {
            case: "ConverseStream with no prefix at all, bucket addressed in the host",
            path: "converse-stream",
            operation: logging::OPERATION_CONVERSE_STREAM,
            key_prefix: None,
            style: Addressing::VirtualHosted,
        },
    ]
}

/// The `loggingConfig` one row installs: the object destination, with text data delivery on
/// and the two payload switches a client that logs text alone turns off.
fn logging_config(key_prefix: Option<&str>) -> Value {
    let mut s3_config = json!({ "bucketName": BUCKET });
    if let Some(prefix) = key_prefix {
        s3_config["keyPrefix"] = json!(prefix);
    }
    json!({
        "s3Config": s3_config,
        "textDataDeliveryEnabled": true,
        "imageDataDeliveryEnabled": false,
        "embeddingDataDeliveryEnabled": false,
    })
}

/// Install `config` through the control plane and read it back, as an operator does before
/// expecting records.
async fn given_logging(router: &MultiServiceRouter, case: &str, config: &Value) {
    let put = bedrock(
        router,
        "PUT",
        Some(json!({ "loggingConfig": config.clone() })),
    )
    .await;
    assert_eq!(
        put.status,
        200,
        "case {case:?}: the configuration must be admitted, got {} with {}",
        put.status,
        put.json()
    );
    let echoed = bedrock(router, "GET", None).await;
    assert_eq!(
        (
            echoed.status,
            echoed.json()["loggingConfig"]["s3Config"]["bucketName"].as_str(),
        ),
        (200, Some(BUCKET)),
        "case {case:?}: the stored configuration must be echoed back, got {} with {}",
        echoed.status,
        echoed.json()
    );
}

/// Serve one inference on the row's surface, carrying `prompt`, addressed the way an SDK
/// addresses it.
async fn invoke(router: &MultiServiceRouter, case: &str, path: &str, prompt: &str) -> Answer {
    let body = json!({
        "messages": [{ "role": "user", "content": [{ "text": prompt }] }],
        "inferenceConfig": { "maxTokens": 40, "temperature": 0.0 },
    });
    let answered = send(
        router,
        "bedrock",
        "bedrock-runtime",
        "POST",
        &format!("/model/{MODEL_LABEL}/{path}"),
        "application/json",
        serde_json::to_vec(&body).expect("the test body serializes"),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: the inference must be served, got {} with {:?}",
        answered.status,
        answered.text()
    );
    assert!(
        !answered.request_id.is_empty(),
        "case {case:?}: the response must name the request it answers in {REQUEST_ID_HEADER}"
    );
    answered
}

// ---------------------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------------------

/// RULE: on the assembled, authenticating emulator a served inference delivers its record
/// under the configured destination's own prefix, named by the id the caller was answered
/// under and stating the call it is of.
#[tokio::test]
async fn an_inference_served_by_the_assembled_emulator_delivers_its_record() {
    for row in rows() {
        let case = row.case;
        let router = emulator();
        given_bucket(&router, case, row.style).await;
        given_logging(&router, case, &logging_config(row.key_prefix)).await;

        // A prompt whose text is this case's own, so a record carrying it cannot be one
        // delivered for another call.
        let prompt = format!("repeat exactly this token and nothing else: {case}");
        let answered = invoke(&router, case, row.path, &prompt).await;

        let prefix = row.key_prefix.unwrap_or_default();
        let (key, record) =
            awaited_record(&router, case, row.style, prefix, &answered.request_id).await;
        let layout = logging::record_key_prefix(row.key_prefix, &account_id(), REGION);
        assert!(
            key.starts_with(&layout) && key.ends_with(logging::LOG_FILE_SUFFIX),
            "case {case:?}: expected the record under {layout:?} as a {} object, got {key:?}",
            logging::LOG_FILE_SUFFIX
        );
        assert_eq!(
            (
                record["operation"].as_str(),
                record["modelId"].as_str(),
                record["region"].as_str(),
                record["accountId"].as_str(),
            ),
            (
                Some(row.operation),
                Some(MODEL),
                Some(REGION),
                Some(account_id().as_str()),
            ),
            "case {case:?}: the record must state the call it is of, got {record}"
        );
        assert!(
            record.to_string().contains(&prompt),
            "case {case:?}: text data delivery is on, so the record carries the prompt, got \
             {record}"
        );
    }
}

/// RULE: the delete stops the records and a re-install resumes them without flushing the
/// invocation served while none was installed — observed on the assembled emulator.
#[tokio::test]
async fn the_delete_withholds_the_records_the_re_install_does_not_flush() {
    let case = "logging is withdrawn between three calls and then re-installed";
    let prefix = "sim2real";
    // The bucket a stock client holds and reads: addressed in the host, as an AWS SDK
    // addresses one by default.
    let style = Addressing::VirtualHosted;
    let router = emulator();
    given_bucket(&router, case, style).await;
    given_logging(&router, case, &logging_config(Some(prefix))).await;

    // Leg A, the positive control: the call served while the configuration stands.
    let logged = invoke(&router, case, "converse", "the call before the delete").await;
    awaited_record(&router, case, style, prefix, &logged.request_id).await;

    let deleted = bedrock(&router, "DELETE", None).await;
    assert_eq!(
        deleted.status,
        200,
        "case {case:?}: the configuration must be removable, got {} with {}",
        deleted.status,
        deleted.json()
    );
    let echoed = bedrock(&router, "GET", None).await;
    assert_eq!(
        (echoed.status, echoed.json()["loggingConfig"].clone()),
        (200, Value::Null),
        "case {case:?}: the read must report no configuration, got {} with {}",
        echoed.status,
        echoed.json()
    );

    // Leg B: the call served while nothing is installed is withheld.
    let withheld = invoke(&router, case, "converse", "the call while logging is off").await;
    let after_delete = delivered_records(&router, case, style, prefix).await;
    assert_eq!(
        after_delete.len(),
        1,
        "case {case:?}: expected the positive control alone, got {:?}",
        after_delete
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>()
    );

    // Leg C: the re-install resumes delivery for the calls served after it, and flushes
    // nothing that was withheld.
    given_logging(&router, case, &logging_config(Some(prefix))).await;
    let resumed = invoke(&router, case, "converse", "the call after the re-put").await;
    awaited_record(&router, case, style, prefix, &resumed.request_id).await;
    let records = delivered_records(&router, case, style, prefix).await;
    // The two delivered records, as a SET: two records of one minute are told apart by the
    // id each call was answered under, which is not an order anything promises.
    let mut named: Vec<String> = records
        .iter()
        .filter_map(|(_, record)| record["requestId"].as_str().map(str::to_string))
        .collect();
    named.sort();
    let mut expected = vec![logged.request_id.clone(), resumed.request_id.clone()];
    expected.sort();
    assert_eq!(
        named, expected,
        "case {case:?}: expected the two logged calls and not the withheld {:?}, got {named:?}",
        withheld.request_id
    );
}
