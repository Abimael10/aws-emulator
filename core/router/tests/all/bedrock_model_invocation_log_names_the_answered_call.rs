// SPDX-License-Identifier: CC0-1.0

//! A delivered model-invocation log record names the CALL it is of, across the two Bedrock
//! planes and the object store the records land in.
//!
//! RULE: while an account's model invocation logging is configured, the record every served
//! inference delivers names that call by the request id the CALLER was answered under (the
//! `x-amzn-RequestId` of that very response) and names the principal the call was made by
//! (the ARN `sts:GetCallerIdentity` answers for the credential it was signed with) — so a
//! consumer holding a response, or auditing an identity, finds the record of that one
//! invocation instead of guessing which record of an operation is its own. A configuration
//! that withholds a datatype withholds that datatype's bodies and nothing else: the record
//! still arrives and is still found by the id the caller holds.
//!
//! Three services have to agree for that to be observable — the Bedrock control plane that
//! stores the configuration, the Bedrock Runtime data plane that serves the inference and
//! names the request its response answers, and the Amazon S3 whose bucket the record is
//! written into, read back here through S3's own `ListObjectsV2`/`GetObject` — plus the
//! token service the record's identity is held against. That is why this suite lives here
//! rather than in one service's crate; the record's own layout and the delivery's
//! destination wiring are pinned by the two service crates' own suites.
//!
//! The rows span what the rule generalises over: the surface that served the inference (the
//! unary and streaming Converse APIs and the unary and streaming invoke APIs, which is the
//! whole set of inference surfaces), the prefix the destination is configured with, and the
//! per-datatype delivery switch that decides how much of a record travels.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state::invocation_logging as logging;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for, which is the Region the records are written
/// from and the one their keys are partitioned by.
const REGION: &str = "us-east-1";

/// The bucket every destination below delivers into. Created with no location constraint,
/// which places it in this suite's own Region.
const BUCKET: &str = "bedrock-invocation-log-records";

/// A model every inference surface serves.
const MODEL: &str = "amazon.nova-lite-v1:0";

/// The URI the three model-invocation-logging operations are bound to.
const LOGGING_URI: &str = "/logging/modelinvocations";

/// The access key id every call below is signed with: a long-term key, so the principal it
/// speaks for is the IAM user this emulator publishes that credential for.
const ACCESS_KEY_ID: &str = "AKIDEXAMPLE";

/// `sts:GetCallerIdentity`'s own API version, for the awsQuery call that reads the caller
/// back.
const STS_API_VERSION: &str = "2011-06-15";

/// The header this protocol reports the request a response answers in.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The account the emulator serves, which every delivered key and record names.
fn account_id() -> String {
    ::engine::emulator_account_id().to_string()
}

// ---------------------------------------------------------------------------------------
// The harness: the two Bedrock planes, the object store and the token service on ONE engine
// ---------------------------------------------------------------------------------------

fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header scoped to `service`, which is what routes the request and
/// what every service reads the caller and the Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One answered call: its status, the id it named the request by, and its body.
struct Answer {
    status: u16,
    request_id: String,
    body: Bytes,
}

impl Answer {
    /// The body as JSON, or `Null` for a body that is not a JSON document (an event
    /// stream, an empty success).
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// One request to the emulator, signed for `service` and addressed at `host`.
async fn send(
    router: &MultiServiceRouter,
    service: &str,
    host: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}.{REGION}.amazonaws.com{path}"))
        .header("host", format!("{host}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(service))
        .header("content-type", content_type)
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
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

/// One restJson1 call to the Bedrock control plane.
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

/// One Amazon S3 request, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str) -> Answer {
    send(router, "s3", "s3", method, path, "application/xml", Vec::new()).await
}

/// `PUT /<bucket>` — the destination bucket a caller holds before configuring logging into
/// it.
async fn given_bucket(router: &MultiServiceRouter, case: &str) {
    let answer = s3(router, "PUT", &format!("/{BUCKET}")).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: the destination bucket must be created, got {} with {:?}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
}

/// Every key the destination bucket holds, as `ListObjectsV2` reports them.
async fn delivered_keys(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let answer = s3(router, "GET", &format!("/{BUCKET}?list-type=2")).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the destination bucket must be listable, got {}",
        answer.status
    );
    let listing = String::from_utf8_lossy(&answer.body).to_string();
    listing
        .split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split("</Key>").next())
        .map(str::to_string)
        .collect()
}

/// The records one delivered object carries, fetched through `GetObject` and read back in
/// the form the delivery wrote them (`logging::delivered_records`).
async fn records_of(router: &MultiServiceRouter, case: &str, key: &str) -> Vec<Value> {
    let answer = s3(router, "GET", &format!("/{BUCKET}/{key}")).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the delivered object {key:?} must be readable, got {}",
        answer.status
    );
    logging::delivered_records(&answer.body).unwrap_or_else(|| {
        panic!(
            "case {case:?}: the delivered object {key:?} is not a vended-log object, got {} \
             byte(s)",
            answer.body.len()
        )
    })
}

/// The `Arn` `sts:GetCallerIdentity` answers for the credential every call here is signed
/// with: the principal a record of one of those calls has to name.
async fn caller_arn(router: &MultiServiceRouter, case: &str) -> String {
    let answer = send(
        router,
        "sts",
        "sts",
        "POST",
        "/",
        "application/x-www-form-urlencoded",
        format!("Action=GetCallerIdentity&Version={STS_API_VERSION}").into_bytes(),
    )
    .await;
    let body = String::from_utf8_lossy(&answer.body).to_string();
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetCallerIdentity must be served, got {} with {body}",
        answer.status
    );
    let arn = body
        .split("<Arn>")
        .nth(1)
        .and_then(|tail| tail.split("</Arn>").next())
        .unwrap_or_else(|| panic!("case {case:?}: GetCallerIdentity answered no <Arn>: {body}"));
    arn.to_string()
}

// ---------------------------------------------------------------------------------------
// The surfaces and the configurations
// ---------------------------------------------------------------------------------------

/// One row: an inference surface, the destination the configuration points at, and how much
/// of each record that configuration lets travel.
struct Row {
    case: &'static str,
    /// The last segment of the inference URI, under `/model/<modelId>/`.
    path: &'static str,
    /// The `operation` the record of a call on this surface has to state.
    operation: &'static str,
    /// Whether the request body is the Converse-API shape rather than the model's own
    /// native invoke body.
    converse_shaped: bool,
    /// `s3Config.keyPrefix`, or `None` for a configuration that names none.
    key_prefix: Option<&'static str>,
    /// `textDataDeliveryEnabled`, which decides whether the prompt and the completion
    /// travel in the record.
    text_data_delivery_enabled: bool,
}

/// The rows: every inference surface, each with its own destination prefix, plus the same
/// surface with text data delivery turned OFF — where the record still has to arrive and
/// still has to be found by the id the caller was answered under.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "Converse, text delivery on, a one-segment prefix",
            path: "converse",
            operation: logging::OPERATION_CONVERSE,
            converse_shaped: true,
            key_prefix: Some("sim2real"),
            text_data_delivery_enabled: true,
        },
        Row {
            case: "Converse, text delivery off, a nested prefix",
            path: "converse",
            operation: logging::OPERATION_CONVERSE,
            converse_shaped: true,
            key_prefix: Some("audit/bedrock"),
            text_data_delivery_enabled: false,
        },
        Row {
            case: "ConverseStream, text delivery on, no prefix at all",
            path: "converse-stream",
            operation: logging::OPERATION_CONVERSE_STREAM,
            converse_shaped: true,
            key_prefix: None,
            text_data_delivery_enabled: true,
        },
        Row {
            case: "InvokeModel, text delivery on, a one-segment prefix",
            path: "invoke",
            operation: logging::OPERATION_INVOKE_MODEL,
            converse_shaped: false,
            key_prefix: Some("sim2real"),
            text_data_delivery_enabled: true,
        },
        Row {
            case: "InvokeModelWithResponseStream, text delivery off, no prefix",
            path: "invoke-with-response-stream",
            operation: logging::OPERATION_INVOKE_MODEL_WITH_RESPONSE_STREAM,
            converse_shaped: false,
            key_prefix: None,
            text_data_delivery_enabled: false,
        },
    ]
}

/// The `loggingConfig` one row installs.
fn logging_config(row: &Row) -> Value {
    let mut s3_config = json!({ "bucketName": BUCKET });
    if let Some(prefix) = row.key_prefix {
        s3_config["keyPrefix"] = json!(prefix);
    }
    json!({
        "s3Config": s3_config,
        "textDataDeliveryEnabled": row.text_data_delivery_enabled,
    })
}

/// Serve one inference on the row's surface, carrying `prompt`.
async fn invoke(router: &MultiServiceRouter, row: &Row, prompt: &str) -> Answer {
    let body = if row.converse_shaped {
        json!({ "messages": [{ "role": "user", "content": [{ "text": prompt }] }] })
    } else {
        json!({
            "messages": [{ "role": "user", "content": [{ "text": prompt }] }],
            "inferenceConfig": { "maxTokens": 64 },
        })
    };
    let answer = send(
        router,
        "bedrock",
        "bedrock-runtime",
        "POST",
        &format!("/model/{MODEL}/{}", row.path),
        "application/json",
        serde_json::to_vec(&body).expect("the test body serializes"),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {:?}: the inference must be served, got {} with {:?}",
        row.case,
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer
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
        (echoed.status, echoed.json()["loggingConfig"].clone()),
        (200, config.clone()),
        "case {case:?}: the configuration in force must be the one submitted, got {} with {}",
        echoed.status,
        echoed.json()
    );
}

// ---------------------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------------------

/// RULE: the record a served inference delivers names that call by the id the CALLER was
/// answered under and names the principal the call was made by, whatever surface served it
/// and whatever the configuration lets travel.
#[tokio::test]
async fn a_delivered_record_names_the_call_the_caller_was_answered_for() {
    for row in rows() {
        let case = row.case;
        let router = emulator();
        given_bucket(&router, case).await;
        given_logging(&router, case, &logging_config(&row)).await;
        let caller = caller_arn(&router, case).await;

        // A prompt whose text is this case's own, so a record carrying it cannot be one
        // delivered for another call.
        let prompt = format!("say something about {case} and nothing else");
        let answered = invoke(&router, &row, &prompt).await;
        assert!(
            !answered.request_id.is_empty(),
            "case {case:?}: the response must name the request it answers in \
             {REQUEST_ID_HEADER}"
        );

        let keys = delivered_keys(&router, case).await;
        assert_eq!(
            keys.len(),
            1,
            "case {case:?}: expected exactly one delivered record, got {keys:?}"
        );
        let key = &keys[0];
        let prefix = logging::record_key_prefix(row.key_prefix, &account_id(), REGION);
        assert!(
            key.starts_with(&prefix) && key.ends_with(logging::LOG_FILE_SUFFIX),
            "case {case:?}: expected the record under {prefix:?} as a \
             {} object, got {key:?}",
            logging::LOG_FILE_SUFFIX
        );
        // The object a consumer polling for one call's record fetches is told apart from
        // another call's by the id that call was answered under.
        assert!(
            key.contains(&answered.request_id),
            "case {case:?}: expected the delivered object to be named for the answered \
             request {:?}, got {key:?}",
            answered.request_id
        );

        let records = records_of(&router, case, key).await;
        assert_eq!(
            records.len(),
            1,
            "case {case:?}: expected one record in {key:?}, got {records:?}"
        );
        let record = &records[0];
        assert_eq!(
            (
                record["requestId"].as_str(),
                record["schemaType"].as_str(),
                record["operation"].as_str(),
                record["modelId"].as_str(),
                record["accountId"].as_str(),
                record["region"].as_str(),
                record["identity"]["arn"].as_str(),
            ),
            (
                Some(answered.request_id.as_str()),
                Some(logging::SCHEMA_TYPE),
                Some(row.operation),
                Some(MODEL),
                Some(account_id().as_str()),
                Some(REGION),
                Some(caller.as_str()),
            ),
            "case {case:?}: the record must name the call the caller was answered for, got \
             {record}"
        );
        // The switch decides the BODIES and nothing else: the record above arrived either
        // way, named by the id the caller holds.
        let carries_the_prompt = record.to_string().contains(&prompt);
        assert_eq!(
            carries_the_prompt, row.text_data_delivery_enabled,
            "case {case:?}: expected the prompt in the record={}, got {record}",
            row.text_data_delivery_enabled
        );
    }
}

/// RULE: the delivery stops with the configuration, and the responses do not change with
/// it: an invocation served after `DeleteModelInvocationLoggingConfiguration` writes no
/// record and is still answered under an id of its own, so no later record can be
/// attributed to it.
#[tokio::test]
async fn an_invocation_served_after_the_delete_writes_no_record_and_still_names_its_request() {
    let case = "the configuration is withdrawn between two identical calls";
    let router = emulator();
    let row = Row {
        case,
        path: "converse",
        operation: logging::OPERATION_CONVERSE,
        converse_shaped: true,
        key_prefix: Some("sim2real"),
        text_data_delivery_enabled: true,
    };
    given_bucket(&router, case).await;
    given_logging(&router, case, &logging_config(&row)).await;

    let logged = invoke(&router, &row, "the call before the delete").await;
    let before = delivered_keys(&router, case).await;
    assert_eq!(
        before.len(),
        1,
        "case {case:?}: expected the positive control's record, got {before:?}"
    );

    let deleted = bedrock(&router, "DELETE", None).await;
    assert_eq!(
        deleted.status,
        200,
        "case {case:?}: the configuration must be removable, got {} with {}",
        deleted.status,
        deleted.json()
    );

    let unlogged = invoke(&router, &row, "the call after the delete").await;
    let after = delivered_keys(&router, case).await;
    assert_eq!(
        after, before,
        "case {case:?}: expected no further record and the delivered one untouched, got \
         {after:?}"
    );
    assert!(
        !unlogged.request_id.is_empty() && unlogged.request_id != logged.request_id,
        "case {case:?}: expected the unlogged call to be answered under an id of its own, \
         got {:?} against {:?}",
        unlogged.request_id,
        logged.request_id
    );
    assert!(
        !before[0].contains(&unlogged.request_id),
        "case {case:?}: expected the withheld call to be named by no delivered record, got \
         {before:?} for {:?}",
        unlogged.request_id
    );
}
