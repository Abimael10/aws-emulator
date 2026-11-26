// SPDX-License-Identifier: CC0-1.0

//! What this service resolves BEFORE it does the work a request asks for — the
//! output destination an asynchronous invocation is submitted against, and the
//! guardrail a request names — end to end over the router.
//!
//! Two rules, each asserted over a SET of inputs rather than over the one request
//! that showed it:
//!
//! 1. **An asynchronous invocation is accepted only against a destination its
//!    output can be delivered to.** The result of an asynchronous invocation is the
//!    artifact it writes to the caller's own Amazon S3 location, so `StartAsyncInvoke`
//!    resolves `outputDataConfig.s3OutputDataConfig.s3Uri` at submit time — before
//!    the payload that would produce the output — and refuses a submission whose
//!    location names a bucket Amazon S3 does not hold with `400 ValidationException`
//!    carrying the service's own sentence for it, minting no job; a location the
//!    store does hold is accepted whatever shape the prefix takes.
//! 2. **A guardrail reference is read before anything else the request says.** A
//!    `guardrailIdentifier` is the id the control plane minted for the guardrail (or
//!    that guardrail's ARN), and the member's `@pattern` admits far more than that —
//!    a guardrail's NAME, for one — so a value this service cannot read as
//!    addressing a guardrail is refused with the service's own sentence for an
//!    unreadable reference, on the apply surface and on the inference surfaces,
//!    WHATEVER else is also wrong with the request. Only a request naming a readable
//!    guardrail is refused on the version that selects one of its revisions, or on
//!    the guardrail being one this account does not hold.
//!
//! Every assertion names its case and the actual value.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice as bedrock;
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state;
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state::guardrail_registry::{
    GuardrailPolicies, PublishedGuardrail, DRAFT_VERSION,
};
use service_com_amazonaws_s3_amazons3 as s3;
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_runtime_inference::{encode_path, get, post_json, send, Call};

/// The bucket the rows below submit against, created through Amazon S3's own
/// `CreateBucket` the way a caller creates its output bucket.
const HELD_BUCKET: &str = "bedrock-submit-destination";

/// A bucket nothing below ever creates, so no invocation configured to write there
/// could deliver its output.
const ABSENT_BUCKET: &str = "bedrock-submit-absent";

/// A model this service serves asynchronous invocation for, with the `modelInput`
/// its own published schema takes.
const ASYNC_MODEL: &str = "luma.ray-v2:0";

/// The guardrail the reference rows resolve against: an id of the width the control
/// plane mints ([`state::GUARDRAIL_ID_LENGTH`]).
const GUARDRAIL_ID: &str = "abcd1234efgh";

/// The `message` member of a refusal.
fn message_of(call: &Call) -> String {
    call.json()["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// RULE 1: the output destination is resolved before the invocation is accepted
// ---------------------------------------------------------------------------

/// One submission: the location it configures, the `modelInput` it carries, and
/// whether the destination is one this emulator's Amazon S3 can be written to.
struct DestinationRow {
    case: &'static str,
    s3_uri: String,
    model_input: Value,
    writable: bool,
}

/// The destination rows.
///
/// The writable ones differ along the SHAPE of a location inside the held bucket —
/// the bucket root, one prefix segment, the same segment with a trailing key
/// separator, and a deep prefix — and the unwritable ones along the shape of a
/// location in a bucket the store does not hold, including a name one character off
/// the held one, so the rule is read off the bucket the location names and not off
/// the prefix beside it. The last row carries BOTH an unwritable destination and a
/// `modelInput` the model's schema refuses, which is what pins the ORDER: the
/// destination is resolved first.
fn destination_rows() -> Vec<DestinationRow> {
    let prompt = || json!({ "prompt": "A blue cube rotating slowly." });
    vec![
        DestinationRow {
            case: "the root of a held bucket",
            s3_uri: format!("s3://{HELD_BUCKET}"),
            model_input: prompt(),
            writable: true,
        },
        DestinationRow {
            case: "one prefix segment of a held bucket",
            s3_uri: format!("s3://{HELD_BUCKET}/output"),
            model_input: prompt(),
            writable: true,
        },
        DestinationRow {
            case: "a prefix written with a trailing key separator",
            s3_uri: format!("s3://{HELD_BUCKET}/output/"),
            model_input: prompt(),
            writable: true,
        },
        DestinationRow {
            case: "a deep prefix of a held bucket",
            s3_uri: format!("s3://{HELD_BUCKET}/renders/2026/january"),
            model_input: prompt(),
            writable: true,
        },
        DestinationRow {
            case: "the root of a bucket the store does not hold",
            s3_uri: format!("s3://{ABSENT_BUCKET}"),
            model_input: prompt(),
            writable: false,
        },
        DestinationRow {
            case: "a deep prefix of a bucket the store does not hold",
            s3_uri: format!("s3://{ABSENT_BUCKET}/renders/2026/january"),
            model_input: prompt(),
            writable: false,
        },
        DestinationRow {
            case: "a bucket name one character off the held one",
            s3_uri: format!("s3://{HELD_BUCKET}x/output"),
            model_input: prompt(),
            writable: false,
        },
        DestinationRow {
            case: "an unwritable destination and a modelInput the schema refuses",
            s3_uri: format!("s3://{ABSENT_BUCKET}/output"),
            model_input: json!({ "notAModelInputKey": 1 }),
            writable: false,
        },
    ]
}

/// RULE 1: `StartAsyncInvoke` resolves the configured output location before it
/// accepts a submission, so a location naming a bucket this emulator's Amazon S3
/// does not hold is refused `400 ValidationException` with the service's own
/// sentence and mints no job, while every shape of a location inside a held bucket
/// is accepted and reads back through `GetAsyncInvoke`.
#[tokio::test]
async fn an_output_location_that_cannot_be_written_is_refused_before_a_job_is_minted() {
    let router = emulator_with_object_store();
    create_bucket(&router, HELD_BUCKET).await;

    let mut accepted: Vec<String> = Vec::new();
    for row in destination_rows() {
        let case = row.case;
        let call = post_json(
            &router,
            "/async-invoke",
            &json!({
                "modelId": ASYNC_MODEL,
                "modelInput": row.model_input,
                "outputDataConfig": {"s3OutputDataConfig": {"s3Uri": row.s3_uri}},
            }),
        )
        .await;
        if row.writable {
            assert_eq!(
                call.status,
                200,
                "case {case:?}: expected the submission to be accepted, got {} / {}",
                call.status,
                String::from_utf8_lossy(&call.body)
            );
            let arn = call.json()["invocationArn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no invocationArn in {}", call.json()))
                .to_string();
            // Backed by state: the accepted job reads back through the implemented
            // reader, under its own location inside the one it was submitted against.
            let read_back = get(&router, &format!("/async-invoke/{}", encode_path(&arn))).await;
            assert_eq!(
                read_back.status,
                200,
                "case {case:?}: expected the accepted job to read back, got {} / {}",
                read_back.status,
                String::from_utf8_lossy(&read_back.body)
            );
            let reported = format!(
                "{}/{}",
                row.s3_uri.trim_end_matches('/'),
                arn.rsplit('/').next().unwrap_or_default()
            );
            assert_eq!(
                read_back.json()["outputDataConfig"]["s3OutputDataConfig"]["s3Uri"].as_str(),
                Some(reported.as_str()),
                "case {case:?}: expected the submitted location extended by the minted \
                 invocation id, got {}",
                read_back.json()
            );
            accepted.push(arn);
            continue;
        }
        assert_eq!(
            call.status,
            400,
            "case {case:?}: expected the submission to be refused, got {} / {}",
            call.status,
            String::from_utf8_lossy(&call.body)
        );
        assert_eq!(
            call.error_code(),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            call.error_code()
        );
        assert_eq!(
            message_of(&call),
            state::async_invoke_output::UNWRITABLE_OUTPUT_LOCATION_MESSAGE,
            "case {case:?}: expected {:?}, got {:?}",
            state::async_invoke_output::UNWRITABLE_OUTPUT_LOCATION_MESSAGE,
            message_of(&call)
        );
        assert!(
            call.json()["invocationArn"].is_null(),
            "case {case:?}: a refused submission mints nothing, got {}",
            call.json()
        );
    }

    // Nothing was minted for the refused rows: the service holds exactly the jobs
    // the accepted ones submitted, as its own listing reports them.
    let listed = get(&router, "/async-invoke").await;
    assert_eq!(
        listed.status,
        200,
        "case \"the listing\": expected 200 from ListAsyncInvokes, got {} / {}",
        listed.status,
        String::from_utf8_lossy(&listed.body)
    );
    let summaries = listed.json()["asyncInvokeSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        summaries.len(),
        accepted.len(),
        "case \"the listing\": expected only the accepted submissions to be held, got {}",
        listed.json()
    );
}

// ---------------------------------------------------------------------------
// RULE 2: the guardrail reference is read first
// ---------------------------------------------------------------------------

/// One `ApplyGuardrail` request named by its guardrail reference, and the sentence
/// the service answers it with.
struct ReferenceRow {
    case: &'static str,
    identifier: String,
    version: &'static str,
    message: &'static str,
}

/// The reference rows.
///
/// The unreadable identifiers differ in HOW they are unreadable — a guardrail's
/// name rather than its id, an id one character short of the minted width, one
/// character past it, an ARN of this service carrying a name rather than an id, and
/// an ARN of another of this service's resources — and each is paired with a version
/// the member's own pattern admits, so the row is refused on the identifier and
/// nothing else. The rows after them are the ORDER: the same unreadable identifier
/// beside an unreadable VERSION is still refused on the identifier, while a readable
/// identifier beside an unreadable version is refused on the version, and a
/// readable identifier naming no guardrail of this account is the absent-guardrail
/// refusal.
fn reference_rows() -> Vec<ReferenceRow> {
    let one_short = &GUARDRAIL_ID[..state::GUARDRAIL_ID_LENGTH - 1];
    vec![
        ReferenceRow {
            case: "a guardrail's name rather than its id",
            identifier: "mysupportticketguardrail2026".to_string(),
            version: DRAFT_VERSION,
            message: state::GUARDRAIL_REFERENCE_FORMAT_MESSAGE,
        },
        ReferenceRow {
            case: "an id one character short of the minted width",
            identifier: one_short.to_string(),
            version: DRAFT_VERSION,
            message: state::GUARDRAIL_REFERENCE_FORMAT_MESSAGE,
        },
        ReferenceRow {
            case: "an id one character past the minted width",
            identifier: format!("{GUARDRAIL_ID}9"),
            version: "1",
            message: state::GUARDRAIL_REFERENCE_FORMAT_MESSAGE,
        },
        ReferenceRow {
            case: "an ARN of this service carrying a name rather than an id",
            identifier: guardrail_arn("mysupportticketguardrail2026"),
            version: DRAFT_VERSION,
            message: state::GUARDRAIL_REFERENCE_FORMAT_MESSAGE,
        },
        ReferenceRow {
            case: "an ARN of another of this service's resources",
            identifier: engine::emulator_arn(
                "bedrock",
                "us-east-1",
                "async-invoke/abcdef123456",
            ),
            version: DRAFT_VERSION,
            message: state::GUARDRAIL_REFERENCE_FORMAT_MESSAGE,
        },
        ReferenceRow {
            case: "a name and a version that is neither a number nor the draft",
            identifier: "mysupportticketguardrail2026".to_string(),
            version: "not-a-version",
            message: state::GUARDRAIL_REFERENCE_FORMAT_MESSAGE,
        },
        ReferenceRow {
            case: "a name and a version numbered from zero",
            identifier: "mysupportticketguardrail2026".to_string(),
            version: "0",
            message: state::GUARDRAIL_REFERENCE_FORMAT_MESSAGE,
        },
        ReferenceRow {
            case: "the published id and a version that is neither a number nor the draft",
            identifier: GUARDRAIL_ID.to_string(),
            version: "not-a-version",
            message: state::INVALID_REQUEST_MESSAGE,
        },
        ReferenceRow {
            case: "the published id and a version numbered from zero",
            identifier: GUARDRAIL_ID.to_string(),
            version: "0",
            message: state::INVALID_REQUEST_MESSAGE,
        },
        ReferenceRow {
            case: "a readable id this account holds no guardrail under",
            identifier: "zzzz9999yyyy".to_string(),
            version: DRAFT_VERSION,
            message: state::guardrail_registry::GUARDRAIL_NOT_FOUND,
        },
        ReferenceRow {
            case: "the published guardrail's ARN, a version never cut",
            identifier: guardrail_arn(GUARDRAIL_ID),
            version: "9",
            message: state::guardrail_registry::GUARDRAIL_NOT_FOUND,
        },
    ]
}

/// RULE 2, on the apply surface: the guardrail reference is read before the version
/// and before the guardrail is looked for, so a value this service cannot read as
/// addressing a guardrail is refused `400 ValidationException` with the service's
/// own sentence for an unreadable reference whatever else is also wrong — and the
/// guardrail that IS readable and published still applies.
#[tokio::test]
async fn a_guardrail_reference_the_service_cannot_read_is_refused_on_the_reference() {
    let (engine, router) = emulator_with_published_guardrail();

    for row in reference_rows() {
        let case = row.case;
        let call = post_json(
            &router,
            &format!(
                "/guardrail/{}/version/{}/apply",
                encode_path(&row.identifier),
                row.version
            ),
            &json!({ "source": "INPUT", "content": [{ "text": { "text": "Is this safe?" } }] }),
        )
        .await;
        assert_eq!(
            call.status,
            400,
            "case {case:?}: expected 400, got {} / {}",
            call.status,
            String::from_utf8_lossy(&call.body)
        );
        assert_eq!(
            call.error_code(),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            call.error_code()
        );
        assert_eq!(
            message_of(&call),
            row.message,
            "case {case:?}: expected {:?}, got {:?}",
            row.message,
            message_of(&call)
        );
    }

    // The POSITIVE control the rule must not swallow: the published guardrail is
    // addressed by both spellings of a readable reference and is applied.
    for (case, identifier) in [
        ("the published guardrail by its id", GUARDRAIL_ID.to_string()),
        ("the published guardrail by its ARN", guardrail_arn(GUARDRAIL_ID)),
    ] {
        let call = post_json(
            &router,
            &format!(
                "/guardrail/{}/version/{DRAFT_VERSION}/apply",
                encode_path(&identifier)
            ),
            &json!({ "source": "INPUT", "content": [{ "text": { "text": "Is this safe?" } }] }),
        )
        .await;
        assert_eq!(
            call.status,
            200,
            "case {case:?}: expected the apply to be served, got {} / {}",
            call.status,
            String::from_utf8_lossy(&call.body)
        );
        assert_eq!(
            call.json()["action"].as_str(),
            Some("NONE"),
            "case {case:?}: expected the guardrail to report its action, got {}",
            call.json()
        );
    }
    drop(engine);
}

/// RULE 2, on the inference surface: `InvokeModel` carries the same guardrail
/// reference in its headers, so a value this service cannot read as addressing a
/// guardrail is refused there in the same words — and a readable one is not refused
/// on the reference at all.
#[tokio::test]
async fn the_guardrail_reference_rule_holds_on_the_inference_surface() {
    let (engine, router) = emulator_with_published_guardrail();

    // (case, the identifier header, whether the reference is readable)
    let rows: Vec<(&str, String, bool)> = vec![
        (
            "a guardrail's name rather than its id",
            "mysupportticketguardrail2026".to_string(),
            false,
        ),
        (
            "an id one character short of the minted width",
            GUARDRAIL_ID[..state::GUARDRAIL_ID_LENGTH - 1].to_string(),
            false,
        ),
        (
            "an ARN of this service carrying a name rather than an id",
            guardrail_arn("mysupportticketguardrail2026"),
            false,
        ),
        (
            "the published guardrail's id",
            GUARDRAIL_ID.to_string(),
            true,
        ),
        (
            "the published guardrail's ARN",
            guardrail_arn(GUARDRAIL_ID),
            true,
        ),
    ];

    for (case, identifier, readable) in rows {
        let call = send(
            &router,
            "POST",
            "/model/amazon.nova-lite-v1:0/invoke",
            &[
                ("content-type", "application/json"),
                ("x-amzn-bedrock-guardrailidentifier", identifier.as_str()),
                ("x-amzn-bedrock-guardrailversion", DRAFT_VERSION),
            ],
            serde_json::to_vec(&json!({
                "messages": [{"role": "user", "content": [{"text": "Name one planet."}]}],
            }))
            .expect("a serializable body"),
        )
        .await;
        if readable {
            assert_eq!(
                call.status,
                200,
                "case {case:?}: a readable reference is not refused on the reference, got {} / {}",
                call.status,
                String::from_utf8_lossy(&call.body)
            );
            continue;
        }
        assert_eq!(
            call.status,
            400,
            "case {case:?}: expected 400, got {} / {}",
            call.status,
            String::from_utf8_lossy(&call.body)
        );
        assert_eq!(
            message_of(&call),
            state::GUARDRAIL_REFERENCE_FORMAT_MESSAGE,
            "case {case:?}: expected {:?}, got {:?}",
            state::GUARDRAIL_REFERENCE_FORMAT_MESSAGE,
            message_of(&call)
        );
    }
    drop(engine);
}

// ---------------------------------------------------------------------------
// Harnesses
// ---------------------------------------------------------------------------

/// The emulator with this service AND Amazon S3 on one shared engine, behind the
/// production router — the way `core/router` assembles them, which is what lets the
/// submit stage resolve a destination against the store that owns it.
fn emulator_with_object_store() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(bedrock::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(s3::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// The emulator with this service alone, carrying the guardrail the Bedrock control
/// plane would have published — through the very function that plane publishes one
/// with.
fn emulator_with_published_guardrail() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(bedrock::EmulatedService::new(Arc::clone(&engine)));
    assert!(
        state::guardrail_registry::publish(
            &*engine,
            PublishedGuardrail {
                id: GUARDRAIL_ID.to_string(),
                arn: guardrail_arn(GUARDRAIL_ID),
                revisions: BTreeMap::from([(
                    DRAFT_VERSION.to_string(),
                    GuardrailPolicies {
                        blocked_input_messaging: "Sorry, the model cannot answer this question."
                            .to_string(),
                        blocked_outputs_messaging:
                            "Sorry, the model cannot answer with this text.".to_string(),
                        ..GuardrailPolicies::default()
                    },
                )]),
            }
        ),
        "the guardrail must be publishable on the engine the service holds"
    );
    (engine, router)
}

/// An ARN of this account and region naming the guardrail `id`, as the control
/// plane mints one.
fn guardrail_arn(id: &str) -> String {
    engine::emulator_arn("bedrock", "us-east-1", &format!("guardrail/{id}"))
}

/// Create `bucket` through Amazon S3's own `CreateBucket`, the way a caller creates
/// the bucket it submits an invocation against.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("https://s3.us-east-1.amazonaws.com/{bucket}"))
        .header("host", "s3.us-east-1.amazonaws.com")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR000000000/20260101/us-east-1/s3/\
             aws4_request, SignedHeaders=host, Signature=0000",
        )
        .body(bytes::Bytes::new())
        .expect("a well-formed S3 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert!(
        (200..300).contains(&status),
        "the output bucket {bucket:?} must be created, got {status}"
    );
}
