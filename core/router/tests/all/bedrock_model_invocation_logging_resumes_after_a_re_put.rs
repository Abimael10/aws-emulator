// SPDX-License-Identifier: CC0-1.0

//! Model invocation logging delivers for the configuration IN FORCE when an inference is
//! served — and for no other.
//!
//! RULE: while a model-invocation-logging configuration is installed, every inference the
//! Bedrock Runtime data plane serves delivers one record to the destination that
//! configuration names; an inference served while no configuration is installed delivers
//! nothing and is never recorded later, so re-installing a configuration resumes delivery
//! for the invocations served AFTER it and flushes none of the invocations served while
//! none was installed.
//!
//! That lifecycle is what makes the compliance-relevant half of invocation logging
//! observable: a consumer can tell "logging is configured" from "logging works", and can
//! establish that the calls made with logging off were never recorded — rather than
//! buffered somewhere and delivered when logging came back.
//!
//! Three services have to agree for it to be observable — the Bedrock control plane that
//! stores, removes and re-stores the configuration, the Bedrock Runtime data plane that
//! serves the inferences and names the request each response answers, and the Amazon S3
//! whose bucket the records land in, read back here through S3's own
//! `ListObjectsV2`/`GetObject`. That is why this suite lives here rather than in one
//! service's crate; a record's own layout and contents are pinned by the two service
//! crates' own suites, and `tests/all/bedrock_model_invocation_log_names_the_answered_call.rs`
//! pins which call a delivered record names.
//!
//! The rows span what the rule generalises over: the inference surface that served the
//! calls, how many invocations were served while no configuration was installed, the
//! destination prefix the re-install names (the same one, another one, or none at all) and
//! the per-datatype delivery switch the configuration carries — a record arrives for every
//! one of them.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state::invocation_logging as logging;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for, which is the Region the records are
/// written from and the one their keys are partitioned by.
const REGION: &str = "us-east-1";

/// The bucket every destination below delivers into. Created with no location constraint,
/// which places it in this suite's own Region.
const BUCKET: &str = "bedrock-invocation-log-lifecycle";

/// A model every inference surface serves.
const MODEL: &str = "amazon.nova-lite-v1:0";

/// The URI the three model-invocation-logging operations are bound to.
const LOGGING_URI: &str = "/logging/modelinvocations";

/// The access key id every call below is signed with.
const ACCESS_KEY_ID: &str = "AKIDEXAMPLE";

/// The header this protocol reports the request a response answers in.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The account the emulator serves, which every delivered key names.
fn account_id() -> String {
    ::engine::emulator_account_id().to_string()
}

// ---------------------------------------------------------------------------------------
// The harness: the two Bedrock planes and the object store on ONE engine
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
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header scoped to `service`, which is what routes the request
/// and what every service reads the caller and the Region from.
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

/// One restJson1 call to the Bedrock control plane's logging surface.
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

/// Every record the destination bucket holds, each beside the key of the object it was
/// delivered in — fetched through `GetObject` and read back in the form the delivery wrote
/// them (`logging::delivered_records`).
async fn delivered_records(router: &MultiServiceRouter, case: &str) -> Vec<(String, Value)> {
    let mut delivered = Vec::new();
    for key in delivered_keys(router, case).await {
        let answer = s3(router, "GET", &format!("/{BUCKET}/{key}")).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the delivered object {key:?} must be readable, got {}",
            answer.status
        );
        let records = logging::delivered_records(&answer.body).unwrap_or_else(|| {
            panic!(
                "case {case:?}: the delivered object {key:?} is not a vended-log object, got {} \
                 byte(s)",
                answer.body.len()
            )
        });
        for record in records {
            delivered.push((key.clone(), record));
        }
    }
    delivered
}

/// The request ids every delivered record names its call by, in a stable order — the
/// records are read in the order the object store lists their keys, which is the keys' own
/// order rather than the order the calls were served in.
fn named_request_ids(delivered: &[(String, Value)]) -> Vec<String> {
    let mut named: Vec<String> = delivered
        .iter()
        .map(|(_, record)| record["requestId"].as_str().unwrap_or_default().to_string())
        .collect();
    named.sort();
    named
}

// ---------------------------------------------------------------------------------------
// The surfaces and the configurations
// ---------------------------------------------------------------------------------------

/// One row: the inference surface the three legs are served on, the destination the
/// configuration names before and after it is withdrawn, and how many invocations are
/// served while none is installed.
struct Row {
    case: &'static str,
    /// The last segment of the inference URI, under `/model/<modelId>/`.
    path: &'static str,
    /// The `operation` the record of a call on this surface has to state.
    operation: &'static str,
    /// Whether the request body is the Converse-API shape rather than the model's own
    /// native invoke body.
    converse_shaped: bool,
    /// `s3Config.keyPrefix` of the configuration installed first.
    key_prefix: Option<&'static str>,
    /// `s3Config.keyPrefix` of the configuration installed again after the delete.
    re_put_key_prefix: Option<&'static str>,
    /// How many invocations are served while no configuration is installed.
    withheld_invocations: usize,
    /// `textDataDeliveryEnabled`, which decides how much of a record travels — never
    /// whether one arrives.
    text_data_delivery_enabled: bool,
}

/// The rows: the unary and streaming Converse surfaces and the unary invoke surface, each
/// with its own withheld count, its own destination prefix on the way back and its own
/// delivery switch.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "Converse, re-put at the same prefix after one withheld call",
            path: "converse",
            operation: logging::OPERATION_CONVERSE,
            converse_shaped: true,
            key_prefix: Some("sim2real"),
            re_put_key_prefix: Some("sim2real"),
            withheld_invocations: 1,
            text_data_delivery_enabled: true,
        },
        Row {
            case: "InvokeModel, re-put at another prefix after two withheld calls",
            path: "invoke",
            operation: logging::OPERATION_INVOKE_MODEL,
            converse_shaped: false,
            key_prefix: Some("sim2real"),
            re_put_key_prefix: Some("audit/bedrock"),
            withheld_invocations: 2,
            text_data_delivery_enabled: true,
        },
        Row {
            case: "ConverseStream, text delivery off, re-put with no prefix after three \
                   withheld calls",
            path: "converse-stream",
            operation: logging::OPERATION_CONVERSE_STREAM,
            converse_shaped: true,
            key_prefix: Some("audit/bedrock"),
            re_put_key_prefix: None,
            withheld_invocations: 3,
            text_data_delivery_enabled: false,
        },
    ]
}

/// The `loggingConfig` one leg installs: the destination bucket under `key_prefix`, with
/// the row's delivery switch.
fn logging_config(row: &Row, key_prefix: Option<&str>) -> Value {
    let mut s3_config = json!({ "bucketName": BUCKET });
    if let Some(prefix) = key_prefix {
        s3_config["keyPrefix"] = json!(prefix);
    }
    json!({
        "s3Config": s3_config,
        "textDataDeliveryEnabled": row.text_data_delivery_enabled,
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
        (echoed.status, echoed.json()["loggingConfig"].clone()),
        (200, config.clone()),
        "case {case:?}: the configuration in force must be the one submitted, got {} with {}",
        echoed.status,
        echoed.json()
    );
}

/// Withdraw the configuration and confirm the control plane reports none in force, which is
/// what a client reads before expecting no further record.
async fn given_no_logging(router: &MultiServiceRouter, case: &str) {
    let deleted = bedrock(router, "DELETE", None).await;
    assert_eq!(
        deleted.status,
        200,
        "case {case:?}: the configuration must be removable, got {} with {}",
        deleted.status,
        deleted.json()
    );
    let echoed = bedrock(router, "GET", None).await;
    assert_eq!(
        (echoed.status, echoed.json()["loggingConfig"].clone()),
        (200, Value::Null),
        "case {case:?}: expected no configuration in force, got {} with {}",
        echoed.status,
        echoed.json()
    );
}

/// Serve one inference on the row's surface, carrying `prompt`, and answer the id the
/// caller was answered under.
async fn invoke(router: &MultiServiceRouter, row: &Row, case: &str, prompt: &str) -> String {
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
        "case {case:?}: the inference must be served, got {} with {:?}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert!(
        !answer.request_id.is_empty(),
        "case {case:?}: the response must name the request it answers in {REQUEST_ID_HEADER}"
    );
    answer.request_id
}

// ---------------------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------------------

/// RULE: delivery follows the configuration in force at the time of the invocation — the
/// calls served while one is installed are delivered, the calls served while none is
/// installed are delivered never, and installing one again delivers the calls that come
/// after it without flushing the ones that came before it.
#[tokio::test]
async fn re_installing_invocation_logging_delivers_later_calls_and_flushes_no_withheld_one() {
    for row in rows() {
        let case = row.case;
        let router = emulator();
        given_bucket(&router, case).await;

        // The positive control: with a configuration installed, the call that was served
        // is delivered, under the prefix that configuration named.
        given_logging(&router, case, &logging_config(&row, row.key_prefix)).await;
        let logged = invoke(&router, &row, case, "the call before the delete").await;
        let delivered = delivered_records(&router, case).await;
        assert_eq!(
            (delivered.len(), named_request_ids(&delivered)),
            (1, vec![logged.clone()]),
            "case {case:?}: expected the served call's own record, got {delivered:?}"
        );
        let installed_prefix = logging::record_key_prefix(row.key_prefix, &account_id(), REGION);
        assert!(
            delivered[0].0.starts_with(&installed_prefix),
            "case {case:?}: expected the record under {installed_prefix:?}, got {:?}",
            delivered[0].0
        );
        assert_eq!(
            delivered[0].1["operation"].as_str(),
            Some(row.operation),
            "case {case:?}: the record must state the surface that served the call, got {}",
            delivered[0].1
        );

        // The configuration is withdrawn: the calls served from here deliver nothing.
        given_no_logging(&router, case).await;
        let mut withheld = Vec::new();
        for index in 0..row.withheld_invocations {
            withheld.push(
                invoke(
                    &router,
                    &row,
                    case,
                    &format!("the call {index} made while logging was deleted"),
                )
                .await,
            );
        }
        let while_deleted = delivered_records(&router, case).await;
        assert_eq!(
            while_deleted, delivered,
            "case {case:?}: expected no record for the {} withheld call(s) {withheld:?} and the \
             delivered one untouched, got {while_deleted:?}",
            withheld.len()
        );

        // The configuration is installed again: delivery resumes for the call served after
        // it, at whatever destination THAT configuration names, and nothing withheld is
        // flushed behind it.
        given_logging(&router, case, &logging_config(&row, row.re_put_key_prefix)).await;
        let resumed = invoke(&router, &row, case, "the call after the re-put").await;
        let after_re_put = delivered_records(&router, case).await;
        let named = named_request_ids(&after_re_put);
        let mut expected = vec![logged.clone(), resumed.clone()];
        expected.sort();
        assert_eq!(
            (after_re_put.len(), named.clone()),
            (2, expected),
            "case {case:?}: expected the two logged calls' records and no other, got \
             {after_re_put:?}"
        );
        let resumed_key = after_re_put
            .iter()
            .find(|(_, record)| record["requestId"].as_str() == Some(resumed.as_str()))
            .map(|(key, _)| key.clone())
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a record for the resumed call {resumed:?}, got \
                     {after_re_put:?}"
                )
            });
        let resumed_prefix =
            logging::record_key_prefix(row.re_put_key_prefix, &account_id(), REGION);
        assert!(
            resumed_key.starts_with(&resumed_prefix),
            "case {case:?}: expected the resumed call's record under {resumed_prefix:?}, got \
             {resumed_key:?}"
        );
        assert!(
            after_re_put.contains(&delivered[0]),
            "case {case:?}: expected the positive control's record untouched, got \
             {after_re_put:?}"
        );
        for id in &withheld {
            assert!(
                !named.contains(id),
                "case {case:?}: expected the withheld call {id:?} to be named by no record, got \
                 {named:?}"
            );
            assert!(
                !after_re_put.iter().any(|(key, _)| key.contains(id)),
                "case {case:?}: expected no object delivered for the withheld call {id:?}, got \
                 {:?}",
                after_re_put
                    .iter()
                    .map(|(key, _)| key.clone())
                    .collect::<Vec<String>>()
            );
        }
    }
}
