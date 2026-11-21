// SPDX-License-Identifier: CC0-1.0

//! MODEL INVOCATION LOGGING delivery, end to end over HTTP: the record a served
//! inference writes to the destination this account configured.
//!
//! The rule asserted here, over a SET of inputs rather than over the one call that
//! showed it: while an invocation-logging destination is installed, EVERY inference
//! this data plane serves delivers one record under that destination's prefix,
//! carrying the operation that served it, the model it resolved to, the bodies the
//! call carried and the token counts the API reported inline — and while none is
//! installed, or once it has been withdrawn, no record is written at all.
//!
//! The rows differ along the surface that served the inference (the two Converse
//! surfaces and the two invoke surfaces, unary and streaming), along the prefix the
//! destination was configured with, and along the per-datatype delivery switch that
//! decides whether the prompt and the completion travel.
//!
//! Amazon S3 is mounted beside this service on one engine — the way the assembled
//! emulator runs them — because a delivered record is an object of that store, listed
//! and fetched here through Amazon S3's own operations.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice as bedrock;
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state::invocation_logging as logging;
use service_com_amazonaws_s3_amazons3 as s3;
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_runtime_inference::{post_json, send, Call};

/// The Region every request below is signed for, which is the Region a delivered
/// record is written from and the one its key is partitioned by.
const REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header scoped to Amazon S3, which is what routes a
/// request to the object store rather than to this service.
const S3_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEMULATOR000000000/20260101/us-east-1/s3/aws4_request, \
     SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000";

/// The bucket every destination below delivers into.
///
/// `pub(crate)` with the two helpers over it, so the sibling suite about the
/// CloudWatch Logs destination ([`crate::model_invocation_log_cloud_watch_delivery`])
/// reads an object destination through this one reader rather than growing a second
/// copy of it.
pub(crate) const BUCKET: &str = "bedrock-invocation-logs";

/// A model both Converse surfaces and both invoke surfaces serve.
const MODEL: &str = "amazon.nova-lite-v1:0";

/// The account the emulator serves, which every delivered key and record names.
fn account_id() -> String {
    ::engine::emulator_account_id().to_string()
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

/// This service and Amazon S3 on one shared engine, behind the production router.
/// The engine comes back so a test can install the logging destination the control
/// plane's own put installs — this crate cannot call that control plane (it is the
/// crate that depends on THIS one), so the install seam is driven directly.
fn emulator() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(bedrock::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(s3::EmulatedService::new(Arc::clone(&engine)));
    (engine, router)
}

/// One Amazon S3 request on the same router, path-style and scoped to the `s3`
/// signing name.
async fn s3_call(router: &MultiServiceRouter, method: &str, path: &str) -> Call {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", S3_AUTHORIZATION)
        .body(bytes::Bytes::new())
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Call {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// `PUT /<bucket>` — the destination bucket a caller creates before configuring
/// logging into it.
pub(crate) async fn given_bucket(router: &MultiServiceRouter, case: &str) {
    let answer = s3_call(router, "PUT", &format!("/{BUCKET}")).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the destination bucket must be created, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
}

/// Every key the destination bucket holds under `prefix`, as `ListObjectsV2`
/// reports them.
pub(crate) async fn delivered_keys(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
) -> Vec<String> {
    let answer = s3_call(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the destination bucket must be listable, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let listing = String::from_utf8_lossy(&answer.body).to_string();
    listing
        .split("<Key>")
        .skip(1)
        .filter_map(|tail| tail.split("</Key>").next())
        .map(str::to_string)
        .collect()
}

/// The records one delivered object carries: the object fetched through
/// `GetObject`, decompressed, one JSON document per line.
async fn records_of(router: &MultiServiceRouter, case: &str, key: &str) -> Vec<Value> {
    use std::io::Read;

    let answer = s3_call(router, "GET", &format!("/{BUCKET}/{key}")).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the delivered object {key:?} must be readable, got {}",
        answer.status
    );
    let mut decoded = String::new();
    ::flate2::read::GzDecoder::new(answer.body.as_ref())
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

/// The one record the destination holds under `prefix`, with the key it was
/// delivered under.
async fn sole_record(router: &MultiServiceRouter, case: &str, prefix: &str) -> (String, Value) {
    let keys = delivered_keys(router, case, prefix).await;
    assert_eq!(
        keys.len(),
        1,
        "case {case:?}: expected exactly one delivered object under {prefix:?}, got {keys:?}"
    );
    let key = keys[0].clone();
    let records = records_of(router, case, &key).await;
    assert_eq!(
        records.len(),
        1,
        "case {case:?}: expected one record in {key:?}, got {records:?}"
    );
    (key, records[0].clone())
}

/// Put `delivery` in force, the way the control plane's admitted
/// `PutModelInvocationLoggingConfiguration` puts it.
fn given_logging(
    engine: &Arc<engine::InMemoryEngine>,
    key_prefix: Option<&str>,
    text_data_delivery_enabled: bool,
) {
    assert!(
        logging::install(
            &**engine,
            Some(logging::InvocationLogDelivery {
                object_destination: Some(logging::ObjectDestination {
                    bucket: BUCKET.to_string(),
                    key_prefix: key_prefix.map(str::to_string),
                }),
                log_group_destination: None,
                text_data_delivery_enabled,
                image_data_delivery_enabled: true,
                embedding_data_delivery_enabled: true,
            })
        ),
        "the logging destination must install on this engine"
    );
}

// ---------------------------------------------------------------------------
// The surfaces
// ---------------------------------------------------------------------------

/// One inference surface: how a call reaches it, and what the record it delivers
/// has to say it was.
struct Surface {
    case: &'static str,
    operation: &'static str,
    /// The path the call is posted to, under `/model/<modelId>/…`.
    path: &'static str,
    /// Whether the request body is the Converse-API shape (as opposed to the
    /// model's own native invoke body).
    converse_shaped: bool,
}

/// The four surfaces that serve an inference, which is the set the rule covers:
/// the unary and streaming Converse APIs, and the unary and streaming invoke APIs.
fn surfaces() -> Vec<Surface> {
    vec![
        Surface {
            case: "Converse",
            operation: logging::OPERATION_CONVERSE,
            path: "converse",
            converse_shaped: true,
        },
        Surface {
            case: "ConverseStream",
            operation: logging::OPERATION_CONVERSE_STREAM,
            path: "converse-stream",
            converse_shaped: true,
        },
        Surface {
            case: "InvokeModel",
            operation: logging::OPERATION_INVOKE_MODEL,
            path: "invoke",
            converse_shaped: false,
        },
        Surface {
            case: "InvokeModelWithResponseStream",
            operation: logging::OPERATION_INVOKE_MODEL_WITH_RESPONSE_STREAM,
            path: "invoke-with-response-stream",
            converse_shaped: false,
        },
    ]
}

/// Serve one inference on `surface`, carrying `prompt`.
async fn invoke(router: &MultiServiceRouter, surface: &Surface, prompt: &str) -> Call {
    let body = if surface.converse_shaped {
        json!({ "messages": [{ "role": "user", "content": [{ "text": prompt }] }] })
    } else {
        json!({
            "messages": [{ "role": "user", "content": [{ "text": prompt }] }],
            "inferenceConfig": { "maxTokens": 64 },
        })
    };
    let call = if surface.converse_shaped {
        post_json(router, &format!("/model/{MODEL}/{}", surface.path), &body).await
    } else {
        send(
            router,
            "POST",
            &format!("/model/{MODEL}/{}", surface.path),
            &[("content-type", "application/json")],
            serde_json::to_vec(&body).expect("a serializable test body"),
        )
        .await
    };
    assert_eq!(
        call.status,
        200,
        "case {:?}: the inference must be served, got {} / {}",
        surface.case,
        call.status,
        String::from_utf8_lossy(&call.body)
    );
    call
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// RULE: every inference surface delivers one record for the call it served, under
/// the configured destination's own prefix and in the vended-logs layout, naming the
/// operation that served it, the model it resolved to and the Region it was signed
/// for — and the record carries the request the call arrived with and the answer it
/// served.
#[tokio::test]
async fn every_inference_surface_delivers_a_record_for_the_call_it_served() {
    for surface in surfaces() {
        let case = surface.case;
        let (engine, router) = emulator();
        given_bucket(&router, case).await;
        given_logging(&engine, Some("sim2real"), true);

        // A prompt whose text is this case's own, so a record carrying it cannot be
        // one delivered for another call.
        let prompt = format!("say something about {case} and nothing else");
        invoke(&router, &surface, &prompt).await;

        let prefix = logging::record_key_prefix(Some("sim2real"), &account_id(), REGION);
        let (key, record) = sole_record(&router, case, &prefix).await;
        assert!(
            key.starts_with(&prefix) && key.ends_with(logging::LOG_FILE_SUFFIX),
            "case {case:?}: expected the record under {prefix:?} as a {} object, got {key:?}",
            logging::LOG_FILE_SUFFIX
        );
        assert_eq!(
            (
                record["schemaType"].as_str(),
                record["operation"].as_str(),
                record["modelId"].as_str(),
                record["region"].as_str(),
                record["accountId"].as_str(),
            ),
            (
                Some(logging::SCHEMA_TYPE),
                Some(surface.operation),
                Some(MODEL),
                Some(REGION),
                Some(account_id().as_str()),
            ),
            "case {case:?}: the record must state the call it is of, got {record}"
        );
        assert!(
            !record["requestId"].as_str().unwrap_or_default().is_empty(),
            "case {case:?}: the record must name the invocation it is of, got {record}"
        );
        // The bodies: the prompt the call carried has to appear in the request body
        // the record reports, and the completion the call served in its answer.
        let input_body = record["input"]["inputBodyJson"].to_string();
        assert!(
            input_body.contains(&prompt),
            "case {case:?}: expected the served prompt in the logged request body, got \
             {input_body}"
        );
        assert!(
            !record["output"]["outputBodyJson"].is_null(),
            "case {case:?}: expected the served answer in the record, got {record}"
        );
    }
}

/// RULE: the token counts a record carries are the ones the API reported INLINE for
/// that same call, so a client cannot be billed and audited for an accounting the
/// response never showed it.
#[tokio::test]
async fn a_record_carries_the_token_counts_the_call_reported_inline() {
    // The two unary surfaces report their accounting in the response body a caller
    // reads, which is what the record is held against here. The rows differ along the
    // envelope that reports it and along the length of the prompt, so the counts
    // differ between them.
    // (case, path, converse-shaped, prompt)
    let rows: [(&str, &str, bool, &str); 3] = [
        ("Converse, a short prompt", "converse", true, "hello"),
        (
            "Converse, a longer prompt",
            "converse",
            true,
            "name one primary colour and then say nothing else at all about it",
        ),
        (
            "InvokeModel, the model's own envelope",
            "invoke",
            false,
            "name one primary colour",
        ),
    ];
    for (case, path, converse_shaped, prompt) in rows {
        let (engine, router) = emulator();
        given_bucket(&router, case).await;
        given_logging(&engine, None, true);

        let surface = Surface {
            case,
            operation: logging::OPERATION_CONVERSE,
            path,
            converse_shaped,
        };
        let call = invoke(&router, &surface, prompt).await;
        let answered = call.json();
        // Both envelopes report the accounting under the same two members, so one
        // reading covers the rows.
        let inline = (
            answered["usage"]["inputTokens"].as_i64(),
            answered["usage"]["outputTokens"].as_i64(),
        );
        assert!(
            inline.0.is_some() && inline.1.is_some(),
            "case {case:?}: the call must report its accounting inline, got {answered}"
        );

        let prefix = logging::record_key_prefix(None, &account_id(), REGION);
        let (_, record) = sole_record(&router, case, &prefix).await;
        assert_eq!(
            (
                record["input"]["inputTokenCount"].as_i64(),
                record["output"]["outputTokenCount"].as_i64(),
            ),
            inline,
            "case {case:?}: the record must carry the counts the call reported inline \
             ({inline:?}), got {record}"
        );
    }
}

/// RULE: the per-datatype delivery switch decides only whether the prompt and the
/// completion travel — the record itself still arrives, with the operation, the model
/// and the counts — and the configured `keyPrefix` decides where the whole layout
/// lands.
#[tokio::test]
async fn the_destination_decides_where_a_record_lands_and_how_much_of_it_travels() {
    // (case, the configured keyPrefix, text data delivery, the bodies travel)
    let rows: [(&str, Option<&str>, bool, bool); 4] = [
        ("a one-segment prefix, text on", Some("sim2real"), true, true),
        (
            "a one-segment prefix, text off",
            Some("sim2real"),
            false,
            false,
        ),
        ("no prefix, text on", None, true, true),
        (
            "a nested prefix, text off",
            Some("audit/bedrock"),
            false,
            false,
        ),
    ];
    for (case, key_prefix, text_enabled, bodies) in rows {
        let (engine, router) = emulator();
        given_bucket(&router, case).await;
        given_logging(&engine, key_prefix, text_enabled);

        let prompt = format!("a prompt for {case}");
        invoke(&router, &surfaces()[0], &prompt).await;

        let prefix = logging::record_key_prefix(key_prefix, &account_id(), REGION);
        let (key, record) = sole_record(&router, case, &prefix).await;
        assert!(
            key.starts_with(&prefix),
            "case {case:?}: expected the record under {prefix:?}, got {key:?}"
        );
        assert_eq!(
            record["operation"].as_str(),
            Some(logging::OPERATION_CONVERSE),
            "case {case:?}: the record must arrive whatever the switches say, got {record}"
        );
        assert!(
            record["input"]["inputTokenCount"].as_i64().is_some()
                && record["output"]["outputTokenCount"].as_i64().is_some(),
            "case {case:?}: the record must carry its counts whatever the switches say, got \
             {record}"
        );
        assert_eq!(
            (
                !record["input"]["inputBodyJson"].is_null(),
                !record["output"]["outputBodyJson"].is_null(),
            ),
            (bodies, bodies),
            "case {case:?}: expected the bodies present={bodies}, got {record}"
        );
        if !bodies {
            let rendered = record.to_string();
            assert!(
                !rendered.contains(&prompt),
                "case {case:?}: a record with text delivery off must carry no prompt, got \
                 {rendered}"
            );
        }
    }
}

/// RULE: the record a served inference delivers names the call it is of by the id the
/// CALLER was answered under — the `x-amzn-RequestId` of that very response — and names
/// the principal the call was made by, so a consumer holding a response finds the record
/// of that one invocation instead of guessing which record of an operation is its own.
/// The object the record is delivered as is named for that same id, which is what a
/// consumer polling a prefix for one call's record reads.
///
/// The rows differ along the surface that served the inference (all four) and along the
/// per-datatype delivery switch, because a record withholding the bodies still has to be
/// the record of a call a client can name.
#[tokio::test]
async fn a_record_names_the_call_by_the_id_the_caller_was_answered_under() {
    for surface in surfaces() {
        for text_enabled in [true, false] {
            let case = format!("{} with text data delivery={text_enabled}", surface.case);
            let case = case.as_str();
            let (engine, router) = emulator();
            given_bucket(&router, case).await;
            given_logging(&engine, Some("sim2real"), text_enabled);

            let prompt = format!("a prompt served under {case}");
            let answered = invoke(&router, &surface, &prompt).await;
            let answered_as = answered.header("x-amzn-requestid");
            assert!(
                !answered_as.is_empty(),
                "case {case:?}: the response must name the request it answers"
            );

            let prefix = logging::record_key_prefix(Some("sim2real"), &account_id(), REGION);
            let (key, record) = sole_record(&router, case, &prefix).await;
            assert!(
                key.contains(&answered_as),
                "case {case:?}: expected the delivered object to be named for the answered \
                 request {answered_as:?}, got {key:?}"
            );
            assert_eq!(
                (
                    record["requestId"].as_str(),
                    record["operation"].as_str(),
                    record["identity"]["arn"].as_str(),
                ),
                (
                    Some(answered_as.as_str()),
                    Some(surface.operation),
                    Some(service_common::caller_principal::ambient_user_arn().as_str()),
                ),
                "case {case:?}: the record must name the call the caller was answered for and \
                 the principal that made it, got {record}"
            );
        }
    }
}

/// RULE: delivery follows the configuration. An account that configured no
/// destination has nothing written for it, and an account whose configuration was
/// withdrawn has nothing written for the calls it serves after the withdrawal —
/// which is what makes "logging is configured" and "logging works" two observable
/// states rather than one.
#[tokio::test]
async fn nothing_is_delivered_without_a_configuration_and_nothing_further_after_one_is_withdrawn()
{
    // (case, install a destination before the first call, withdraw it after it)
    let rows: [(&str, bool, bool); 3] = [
        ("no destination was ever configured", false, false),
        ("a destination stands", true, false),
        ("the destination was withdrawn", true, true),
    ];
    for (case, installed, withdrawn) in rows {
        let (engine, router) = emulator();
        given_bucket(&router, case).await;
        if installed {
            given_logging(&engine, Some("sim2real"), true);
        }

        invoke(&router, &surfaces()[0], "the first call").await;
        let delivered_first = delivered_keys(&router, case, "").await.len();
        assert_eq!(
            delivered_first,
            usize::from(installed),
            "case {case:?}: expected {} record(s) for the first call, got {delivered_first}",
            usize::from(installed)
        );

        if withdrawn {
            assert!(
                logging::install(&*engine, None),
                "case {case:?}: the destination must be withdrawable"
            );
        }
        invoke(&router, &surfaces()[0], "the second call").await;
        let delivered_second = delivered_keys(&router, case, "").await.len();
        let expected = match (installed, withdrawn) {
            // Withdrawn: the first call's record stands, and the second writes none.
            (true, true) => 1,
            (true, false) => 2,
            _ => 0,
        };
        assert_eq!(
            delivered_second, expected,
            "case {case:?}: expected {expected} record(s) after the second call, got \
             {delivered_second}"
        );
    }
}
