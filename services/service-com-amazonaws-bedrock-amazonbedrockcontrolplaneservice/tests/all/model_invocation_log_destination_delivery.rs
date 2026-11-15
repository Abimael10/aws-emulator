// SPDX-License-Identifier: CC0-1.0

//! MODEL INVOCATION LOGGING, end to end across the two planes: the configuration this
//! control plane stores is the destination the inference plane DELIVERS each served
//! invocation's record to.
//!
//! Storing a configuration and delivering to it are two different observable states, and
//! this suite is about the second one. An admitted
//! `PutModelInvocationLoggingConfiguration` naming an object destination has to leave
//! every subsequent inference writing its own record under that destination's prefix —
//! so an operator who sees the configuration echoed back by
//! `GetModelInvocationLoggingConfiguration` is right to conclude records are being
//! written — and the `DeleteModelInvocationLoggingConfiguration` that withdraws it has to
//! stop them, leaving what was already delivered where it is.
//!
//! The rows differ along the prefix the destination is configured with and along the
//! per-datatype delivery switch the configuration carries, which is what decides how much
//! of a record travels. The record's own layout and contents are asserted by the
//! inference plane's own suite (`model_invocation_log_delivery.rs` of that crate) and by
//! the unit tests of the component that writes it; what is asserted here is the WIRING:
//! the control plane's put and delete decide whether anything is delivered at all.
//!
//! The three services a record travels through are mounted on ONE shared engine, exactly
//! as `core/router` assembles them: this control plane, the Bedrock Runtime data plane
//! that serves the inference, and the Amazon S3 whose bucket the record lands in — which
//! is listed and fetched here through Amazon S3's own operations.
//!
//! That harness is this file's and its siblings': the suite that reads a destination the
//! way an audit consumer reads it ([`crate::model_invocation_log_poller_view`]) drives the
//! same three services through it rather than standing a second one up.

use std::io::Read;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state::invocation_logging as logging;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::Reply;

/// The Region every call here is signed for: Amazon S3 places a bucket created with no
/// `LocationConstraint` in us-east-1, which is also the Region a delivered record is
/// written from, so one Region label serves every call.
pub(crate) const REGION: &str = "us-east-1";

/// A SigV4 credential scope naming Amazon S3, which is what routes a request to the object
/// store.
const S3_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/s3/aws4_request, \
     SignedHeaders=host, Signature=0000";

/// A SigV4 credential scope naming the Bedrock Runtime data plane's signing name, which is
/// what routes an inference request to it rather than to this control plane.
pub(crate) const RUNTIME_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/bedrock/aws4_request, \
     SignedHeaders=host, Signature=0000";

/// The bucket every destination below delivers into.
pub(crate) const BUCKET: &str = "example-invocation-logs";

/// The model every inference below is served on.
pub(crate) const MODEL: &str = "amazon.nova-lite-v1:0";

/// The URI the logging configuration is addressed by.
pub(crate) const LOGGING_URI: &str = "/logging/modelinvocations";

/// The account the emulator serves, which every delivered key names.
pub(crate) fn account_id() -> String {
    ::engine::emulator_account_id().to_string()
}

// ---------------------------------------------------------------------------------------
// The harness: this control plane, the inference plane and Amazon S3 on one shared engine.
// ---------------------------------------------------------------------------------------

pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

pub(crate) fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(bedrock::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        ::service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    Emulator { router }
}

impl Emulator {
    /// One request, signed with `authorization` — which is what decides the service it
    /// reaches.
    pub(crate) async fn request(
        &self,
        authorization: &str,
        host: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(&value).expect("a serializable request body")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}.{REGION}.amazonaws.com{path}"))
            .header("host", format!("{host}.{REGION}.amazonaws.com"))
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body: Value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Reply {
            status,
            content_type: None,
            body,
            error: response
                .headers()
                .get("x-amzn-errortype")
                .and_then(|value| value.to_str().ok())
                .map(|value| crate::support::error_code(value).to_string()),
        }
    }

    /// One control-plane call.
    pub(crate) async fn bedrock(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        self.request(RUNTIME_AUTHORIZATION, "bedrock", method, path, body)
            .await
    }

    /// One Converse call on the data plane mounted beside it.
    pub(crate) async fn converse(&self, case: &str, prompt: &str) -> Reply {
        let reply = self
            .request(
                RUNTIME_AUTHORIZATION,
                "bedrock-runtime",
                "POST",
                &format!("/model/{MODEL}/converse"),
                Some(json!({
                    "messages": [{ "role": "user", "content": [{ "text": prompt }] }],
                })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: the inference must be served, got {} {:?}",
            reply.status, reply.body
        );
        reply
    }

    /// One path-style request to the emulator's Amazon S3 front door, whose body is the
    /// object's own bytes rather than JSON.
    pub(crate) async fn s3(&self, method: &str, path: &str) -> (u16, bytes::Bytes) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", S3_AUTHORIZATION)
            .body(bytes::Bytes::new())
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        (response.status().as_u16(), response.body().clone())
    }
}

/// `PUT /<bucket>` — the destination bucket a caller creates before configuring logging
/// into it.
pub(crate) async fn given_bucket(emu: &Emulator, case: &str) {
    let (status, body) = emu.s3("PUT", &format!("/{BUCKET}")).await;
    assert_eq!(
        status,
        200,
        "case {case:?}: the destination bucket must be created, got {status} / {:?}",
        String::from_utf8_lossy(&body)
    );
}

/// Every key the destination bucket holds under `prefix`, as `ListObjectsV2` reports them;
/// an empty `prefix` lists the whole bucket. `prefix` travels in the query string as
/// written, so pass a prefix that needs no percent-encoding.
pub(crate) async fn delivered_keys(emu: &Emulator, case: &str, prefix: &str) -> Vec<String> {
    let (status, body) = emu
        .s3("GET", &format!("/{BUCKET}?list-type=2&prefix={prefix}"))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the destination bucket must be listable, got {status}"
    );
    let listing = String::from_utf8_lossy(&body).to_string();
    listing
        .split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split("</Key>").next())
        .map(str::to_string)
        .collect()
}

/// The records one delivered object carries: fetched through `GetObject`, decompressed,
/// one JSON document per line.
pub(crate) async fn records_of(emu: &Emulator, case: &str, key: &str) -> Vec<Value> {
    let (status, body) = emu.s3("GET", &format!("/{BUCKET}/{key}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the delivered object {key:?} must be readable, got {status}"
    );
    let mut decoded = String::new();
    ::flate2::read::GzDecoder::new(body.as_ref())
        .read_to_string(&mut decoded)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the delivered object {key:?} is not a gzip stream: {error}")
        });
    decoded
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|error| {
                panic!("case {case:?}: a delivered record is not JSON ({error}): {line:?}")
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------------------

/// One configuration to install, and what it makes of the records delivered for the
/// invocations served while it stands.
struct Case {
    case: &'static str,
    /// `s3Config.keyPrefix`, or `None` for a configuration that names none.
    key_prefix: Option<&'static str>,
    /// `textDataDeliveryEnabled`, or `None` for a configuration that names it not at all.
    text_data_delivery_enabled: Option<bool>,
    /// Whether the prompt and the completion travel in the record.
    bodies_travel: bool,
}

/// The configuration table: the rows differ along the prefix the whole layout lands under
/// (none, one segment, a nested one) and along the text-delivery switch — named on, named
/// off, and not named at all, which withholds nothing.
fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "a one-segment prefix with text delivery on",
            key_prefix: Some("sim2real"),
            text_data_delivery_enabled: Some(true),
            bodies_travel: true,
        },
        Case {
            case: "a nested prefix with text delivery off",
            key_prefix: Some("audit/bedrock"),
            text_data_delivery_enabled: Some(false),
            bodies_travel: false,
        },
        Case {
            case: "no prefix at all, so the layout starts at the bucket root",
            key_prefix: None,
            text_data_delivery_enabled: Some(true),
            bodies_travel: true,
        },
        Case {
            case: "a configuration that does not name the switch",
            key_prefix: Some("sim2real"),
            text_data_delivery_enabled: None,
            bodies_travel: true,
        },
    ]
}

/// The `loggingConfig` one case puts.
fn logging_config(case: &Case) -> Value {
    let mut s3_config = json!({ "bucketName": BUCKET });
    if let Some(prefix) = case.key_prefix {
        s3_config["keyPrefix"] = json!(prefix);
    }
    let mut config = json!({ "s3Config": s3_config });
    if let Some(enabled) = case.text_data_delivery_enabled {
        config["textDataDeliveryEnabled"] = json!(enabled);
    }
    config
}

/// RULE: an admitted logging configuration naming an object destination is the destination
/// every invocation served after it delivers its own record to, under that
/// configuration's own `keyPrefix` — and the configuration's per-datatype switches decide
/// only how much of each record travels, never whether one arrives.
#[tokio::test]
async fn an_installed_logging_configuration_is_delivered_to_by_every_invocation() {
    for case in cases() {
        let name = case.case;
        let emu = emulator();
        given_bucket(&emu, name).await;

        let put = emu
            .bedrock("PUT", LOGGING_URI, Some(json!({ "loggingConfig": logging_config(&case) })))
            .await;
        assert_eq!(
            put.status, 200,
            "case {name:?}: the configuration must be admitted, got {} {:?}",
            put.status, put.body
        );
        // The configuration an operator reads back, which is what makes the delivery
        // below the thing that configuration promised.
        let echoed = emu.bedrock("GET", LOGGING_URI, None).await;
        assert_eq!(
            (
                echoed.status,
                echoed.body["loggingConfig"]["s3Config"]["bucketName"].as_str(),
            ),
            (200, Some(BUCKET)),
            "case {name:?}: the stored configuration must be echoed back, got {} {:?}",
            echoed.status,
            echoed.body
        );

        // A prompt whose text is this case's own, so a record carrying it cannot be one
        // delivered for another call.
        let prompt = format!("a prompt served under {name}");
        let answered = emu.converse(name, &prompt).await;

        let prefix = logging::record_key_prefix(case.key_prefix, &account_id(), REGION);
        let keys = delivered_keys(&emu, name, "").await;
        assert_eq!(
            keys.len(),
            1,
            "case {name:?}: expected one delivered record, got {keys:?}"
        );
        assert!(
            keys[0].starts_with(&prefix),
            "case {name:?}: expected the record under {prefix:?}, got {:?}",
            keys[0]
        );
        let records = records_of(&emu, name, &keys[0]).await;
        assert_eq!(
            records.len(),
            1,
            "case {name:?}: expected one record in the object, got {records:?}"
        );
        let record = &records[0];
        assert_eq!(
            (
                record["operation"].as_str(),
                record["modelId"].as_str(),
                record["input"]["inputTokenCount"].as_i64(),
                record["output"]["outputTokenCount"].as_i64(),
            ),
            (
                Some(logging::OPERATION_CONVERSE),
                Some(MODEL),
                answered.body["usage"]["inputTokens"].as_i64(),
                answered.body["usage"]["outputTokens"].as_i64(),
            ),
            "case {name:?}: the record must state the call it is of and the accounting that \
             call reported inline, got {record}"
        );
        let carries_the_prompt = record.to_string().contains(&prompt);
        assert_eq!(
            carries_the_prompt, case.bodies_travel,
            "case {name:?}: expected the prompt in the record={}, got {record}",
            case.bodies_travel
        );
    }
}

/// RULE: the delivery stops with the configuration. Once
/// `DeleteModelInvocationLoggingConfiguration` has withdrawn it, an invocation served
/// afterwards writes no record — and what was already delivered stays where it is.
#[tokio::test]
async fn a_withdrawn_configuration_stops_the_records_and_leaves_the_delivered_ones() {
    let case = "the configuration is withdrawn between two identical calls";
    let emu = emulator();
    given_bucket(&emu, case).await;

    let put = emu
        .bedrock(
            "PUT",
            LOGGING_URI,
            Some(json!({
                "loggingConfig": {
                    "s3Config": { "bucketName": BUCKET, "keyPrefix": "sim2real" },
                    "textDataDeliveryEnabled": true,
                },
            })),
        )
        .await;
    assert_eq!(
        put.status, 200,
        "case {case:?}: the configuration must be admitted, got {} {:?}",
        put.status, put.body
    );

    emu.converse(case, "the call before the delete").await;
    let before = delivered_keys(&emu, case, "").await;
    assert_eq!(
        before.len(),
        1,
        "case {case:?}: expected the positive control's record, got {before:?}"
    );

    let deleted = emu.bedrock("DELETE", LOGGING_URI, None).await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: the configuration must be removable, got {} {:?}",
        deleted.status, deleted.body
    );

    emu.converse(case, "the byte-identical call after the delete")
        .await;
    let after = delivered_keys(&emu, case, "").await;
    assert_eq!(
        after, before,
        "case {case:?}: expected no further record and the delivered one untouched, got \
         {after:?}"
    );
}

/// RULE: an account that configured no destination has nothing delivered for it, so a
/// bucket a caller holds for the purpose stays empty until a configuration points at it.
#[tokio::test]
async fn an_account_with_no_configuration_has_nothing_delivered() {
    let case = "no logging configuration was ever put";
    let emu = emulator();
    given_bucket(&emu, case).await;

    let echoed = emu.bedrock("GET", LOGGING_URI, None).await;
    assert_eq!(
        echoed.status, 200,
        "case {case:?}: the read must be served, got {} {:?}",
        echoed.status, echoed.body
    );

    emu.converse(case, "an unlogged call").await;
    let keys = delivered_keys(&emu, case, "").await;
    assert!(
        keys.is_empty(),
        "case {case:?}: expected nothing delivered, got {keys:?}"
    );
}
