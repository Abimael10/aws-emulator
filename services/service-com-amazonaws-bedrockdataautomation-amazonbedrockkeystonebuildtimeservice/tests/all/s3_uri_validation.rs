// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the S3 LOCATIONS a submission of this service carries:
//! which ones it is accepted with, and how it is refused for the rest.
//!
//! Every case drives the real `restJson1` wire path — `MultiServiceRouter::dispatch` with a
//! signed-looking request — against this service and Amazon S3 mounted on ONE shared engine,
//! the way the assembled emulator runs them, so routing, HTTP binding, the centralized
//! constraint walk, the handler, its in-process call into the object store and response
//! serialization are all exercised, exactly as an AWS SDK `bedrock-data-automation` client
//! would.
//!
//! RULE: a submission is accepted only if the S3 locations it names are THERE. A member the
//! model gives the `S3Object` shape names one object the job READS, so the store has to hold
//! that object; a member modelled as a bare `S3Uri` names the place the job WRITES, so the
//! store has to hold its bucket. A submission naming a location the account's object storage
//! does not hold — or one that addresses no location at all, which no store can hold — is
//! refused `400 ValidationException` with this service's fixed sentence for it, and mints no
//! job. The modelled `@pattern` on `S3Uri` (`smithy_query(shape="S3Uri")`:
//! `^s3://[a-z0-9][\.\-a-z0-9]{1,61}[a-z0-9](/.*)?$`) decides only the SHAPE of the value, so
//! a value outside it is refused by the request boundary instead, with its constraint
//! sentence.
//!
//! The suites are TABLE-driven: one row set spans what the rule generalises over (the object
//! is there, its key is not, its bucket is not, the URI names a bucket and no object, the
//! bucket name is one Amazon S3 does not accept — each published rule the modelled pattern
//! cannot express — and the bucket-name length bounds), and each suite asserts it of every S3
//! member of an operation, under both shapes the model uses.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::state;
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::state::ComAmazonawsBedrockdataautomationAmazonbedrockkeystonebuildtimeserviceState as ServiceState;
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::EmulatedService;
use service_com_amazonaws_s3_amazons3 as s3;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{encode_label, error_code, string_at};

/// The sentence this service states for a submission carrying an S3 location it cannot use:
/// fixed text, with nothing interpolated from the request or from state.
const INVALID_S3_URI: &str = "Request has invalid S3 URI";

/// The region every case signs for, and the region the staged buckets are created in.
const REGION: &str = "us-east-1";

/// The bucket the staged sample objects live in.
const SAMPLE_BUCKET: &str = "bda-samples";

/// A bucket the store holds with NOTHING in it: a place to write, holding no object.
const OUTPUT_BUCKET: &str = "bda-output";

/// A bucket name at the floor of Amazon S3's published length rule (3 characters).
const SHORTEST_BUCKET: &str = "abc";

/// A dotted bucket name, which Amazon S3 accepts and which the modelled pattern admits.
const DOTTED_BUCKET: &str = "bda.samples.west";

/// A bucket name no store here holds.
const ABSENT_BUCKET: &str = "bda-absent";

/// The keys staged in [`SAMPLE_BUCKET`]: two samples' assets and ground truths, plus a key
/// carrying characters a BUCKET name may not, since a key is not held to the bucket's rules.
const STAGED_KEYS: &[&str] = &[
    "assets/invoice-0.pdf",
    "truth/invoice-0.json",
    "assets/invoice-1.pdf",
    "truth/invoice-1.json",
    "Odd Key_1/ASSET.PDF",
];

/// The key staged in every single-key bucket below.
const STAGED_KEY: &str = "key";

/// Bytes for a staged object: a submission names the object, nothing here reads it.
const STAGED_BYTES: &[u8] = b"a staged sample";

/// A SigV4 `Authorization` header for this service's signing name (`bedrock`, per
/// `OPERATIONS.md`): `can_handle` resolves the route key from it.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTKEY/20240101/{REGION}/bedrock/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef"
    )
}

/// One emulator: this service and Amazon S3 on a single shared engine behind the production
/// router, with the sample objects staged the way a caller would have uploaded them before
/// submitting.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    stage_objects(&engine);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(s3::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// An emulator with this service ALONE: no object store to ask about any location.
fn emulator_without_the_object_store() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Create the buckets the rows address and put the staged objects in them.
fn stage_objects(engine: &Arc<engine::InMemoryEngine>) {
    let state = s3::state::service_state(&**engine).expect("the object store on this engine");
    let mut store = state.lock();
    let longest_bucket = longest_bucket();
    for bucket in [
        SAMPLE_BUCKET,
        OUTPUT_BUCKET,
        SHORTEST_BUCKET,
        DOTTED_BUCKET,
        longest_bucket.as_str(),
    ] {
        assert_eq!(
            store.create_bucket(bucket, REGION),
            s3::state::BucketCreation::Created,
            "the staging bucket {bucket:?} must be created"
        );
    }
    let mut staged: Vec<(&str, &str)> = STAGED_KEYS
        .iter()
        .map(|key| (SAMPLE_BUCKET, *key))
        .collect();
    for bucket in [SHORTEST_BUCKET, DOTTED_BUCKET, longest_bucket.as_str()] {
        staged.push((bucket, STAGED_KEY));
    }
    for (bucket, key) in staged {
        let object = s3::state::Object::stored(STAGED_BYTES.to_vec(), None);
        assert_eq!(
            store.put_object(bucket, key, object),
            Some(s3::state::ObjectWrite::Created),
            "the object {key:?} must be staged in {bucket:?}"
        );
    }
}

/// A bucket name at the ceiling of Amazon S3's published length rule (63 characters).
fn longest_bucket() -> String {
    "b".repeat(63)
}

/// Stage `pairs` asset/ground-truth objects in [`SAMPLE_BUCKET`] under the keys
/// [`bulk_sample`] builds, so a submission can name more locations than the emulator
/// resolves.
fn stage_bulk_samples(engine: &Arc<engine::InMemoryEngine>, pairs: usize) {
    let state = s3::state::service_state(&**engine).expect("the object store on this engine");
    let mut store = state.lock();
    for index in 0..pairs {
        for key in [format!("bulk/asset-{index}.pdf"), format!("bulk/truth-{index}.json")] {
            let object = s3::state::Object::stored(STAGED_BYTES.to_vec(), None);
            assert_eq!(
                store.put_object(SAMPLE_BUCKET, &key, object),
                Some(s3::state::ObjectWrite::Created),
                "the bulk object {key:?} must be staged in {SAMPLE_BUCKET:?}"
            );
        }
    }
}

/// One sample naming the staged pair [`stage_bulk_samples`] put at `index`.
fn bulk_sample(index: usize) -> Value {
    json!({
        "assetS3Object": { "s3Uri": format!("s3://{SAMPLE_BUCKET}/bulk/asset-{index}.pdf") },
        "groundTruthS3Object": { "s3Uri": format!("s3://{SAMPLE_BUCKET}/bulk/truth-{index}.json") },
    })
}

/// Dispatch one request and return its status and decoded JSON body (`Value::Null` for an
/// empty body).
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "bedrock-data-automation.amazonaws.com")
        .header("content-type", "application/json")
        .header("authorization", authorization())
        .body(Bytes::from(
            serde_json::to_vec(body).expect("serializable body"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let decoded = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// `CreateBlueprint` (`PUT /blueprints/`), so the submissions below reference a blueprint
/// this store really holds.
async fn create_blueprint(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "blueprintName": name,
        "type": "DOCUMENT",
        "schema": r#"{"class":"invoice"}"#,
    });
    let (status, payload) = call(router, "PUT", "/blueprints/", &body).await;
    assert_eq!(
        status, 201,
        "seed CreateBlueprint {name:?}: expected 201, got {status} with {payload:?}"
    );
    let blueprint = payload
        .get("blueprint")
        .cloned()
        .unwrap_or_else(|| panic!("seed CreateBlueprint {name:?} returned no blueprint"));
    string_at(&blueprint, "blueprintArn", name)
}

/// A data-automation profile ARN inside the modelled `DataAutomationProfileArn` pattern
/// `^arn:aws(|-cn|-us-gov):bedrock:[a-zA-Z0-9-]*:(aws|[0-9]{12}):data-automation-profile/[a-zA-Z0-9-_.]+$`
/// that this service resolves, so no row is refused for its profile.
fn profile_arn() -> String {
    format!("arn:aws:bedrock:{REGION}:000000000000:data-automation-profile/us.data-automation-v1")
}

/// A staged object's URI: one the store really holds, used wherever a row is not exercising
/// the member under test.
fn staged_object() -> String {
    format!("s3://{SAMPLE_BUCKET}/{}", STAGED_KEYS[0])
}

/// One S3 location a row names, and what it is: whether the store holds the OBJECT it
/// addresses (what a member modelled as an `S3Object` needs) and whether it holds the
/// LOCATION it addresses (what a member modelled as a bare `S3Uri` needs).
struct Location {
    case: &'static str,
    uri: String,
    /// Whether the URI addresses an OBJECT at all — a name Amazon S3 accepts as a bucket,
    /// plus a key under it. A URI that addresses none names nothing for any store to hold,
    /// which is decided without asking one.
    addresses_an_object: bool,
    object_is_held: bool,
    location_is_held: bool,
}

/// The row set every suite here runs, spanning what the rule generalises over. Every URI is
/// INSIDE the modelled `S3Uri` pattern, so each row is the service's decision and not the
/// boundary's.
///
/// The bucket-name rows break one published Amazon S3 general-purpose bucket naming rule each
/// (two adjacent periods, a name written as an IP address, the reserved prefixes and
/// suffixes) — the rules the modelled pattern cannot express — plus both ends of the
/// published length rule.
fn locations() -> Vec<Location> {
    vec![
        Location {
            case: "an object the store holds",
            uri: staged_object(),
            addresses_an_object: true,
            object_is_held: true,
            location_is_held: true,
        },
        Location {
            case: "an object whose key carries characters a bucket name may not",
            uri: format!("s3://{SAMPLE_BUCKET}/{}", STAGED_KEYS[4]),
            addresses_an_object: true,
            object_is_held: true,
            location_is_held: true,
        },
        Location {
            case: "an object in a bucket at the floor of the published length rule",
            uri: format!("s3://{SHORTEST_BUCKET}/{STAGED_KEY}"),
            addresses_an_object: true,
            object_is_held: true,
            location_is_held: true,
        },
        Location {
            case: "an object in a bucket at the ceiling of the published length rule",
            uri: format!("s3://{}/{STAGED_KEY}", longest_bucket()),
            addresses_an_object: true,
            object_is_held: true,
            location_is_held: true,
        },
        Location {
            case: "an object in a dotted bucket",
            uri: format!("s3://{DOTTED_BUCKET}/{STAGED_KEY}"),
            addresses_an_object: true,
            object_is_held: true,
            location_is_held: true,
        },
        Location {
            case: "a key the bucket does not hold",
            uri: format!("s3://{SAMPLE_BUCKET}/assets/never-uploaded.pdf"),
            addresses_an_object: true,
            object_is_held: false,
            location_is_held: true,
        },
        Location {
            case: "a bucket the store does not hold",
            uri: format!("s3://{ABSENT_BUCKET}/{}", STAGED_KEYS[0]),
            addresses_an_object: true,
            object_is_held: false,
            location_is_held: false,
        },
        Location {
            case: "a bucket the store holds, with nothing under it",
            uri: format!("s3://{OUTPUT_BUCKET}"),
            addresses_an_object: false,
            object_is_held: false,
            location_is_held: true,
        },
        Location {
            case: "a bucket the store holds, with a bare slash",
            uri: format!("s3://{OUTPUT_BUCKET}/"),
            addresses_an_object: false,
            object_is_held: false,
            location_is_held: true,
        },
        Location {
            case: "two adjacent periods in the bucket name",
            uri: format!("s3://bda..samples/{STAGED_KEY}"),
            addresses_an_object: false,
            object_is_held: false,
            location_is_held: false,
        },
        Location {
            case: "a bucket name written as an IP address",
            uri: format!("s3://192.168.5.4/{STAGED_KEY}"),
            addresses_an_object: false,
            object_is_held: false,
            location_is_held: false,
        },
        Location {
            case: "the reserved xn-- bucket prefix",
            uri: format!("s3://xn--bda-samples/{STAGED_KEY}"),
            addresses_an_object: false,
            object_is_held: false,
            location_is_held: false,
        },
        Location {
            case: "the reserved sthree- bucket prefix",
            uri: format!("s3://sthree-bda-samples/{STAGED_KEY}"),
            addresses_an_object: false,
            object_is_held: false,
            location_is_held: false,
        },
        Location {
            case: "the reserved amzn-s3-demo- bucket prefix",
            uri: format!("s3://amzn-s3-demo-bucket/{STAGED_KEY}"),
            addresses_an_object: false,
            object_is_held: false,
            location_is_held: false,
        },
        Location {
            case: "the reserved -s3alias bucket suffix",
            uri: format!("s3://bda-samples-s3alias/{STAGED_KEY}"),
            addresses_an_object: false,
            object_is_held: false,
            location_is_held: false,
        },
        Location {
            case: "the reserved --ol-s3 bucket suffix",
            uri: format!("s3://bda-samples--ol-s3/{STAGED_KEY}"),
            addresses_an_object: false,
            object_is_held: false,
            location_is_held: false,
        },
        Location {
            case: "the reserved .mrap bucket suffix",
            uri: format!("s3://bda-samples.mrap/{STAGED_KEY}"),
            addresses_an_object: false,
            object_is_held: false,
            location_is_held: false,
        },
    ]
}

/// Which S3 member of `InvokeBlueprintOptimizationAsync` a row puts its URI in: the model
/// gives the operation three (`samples[].assetS3Object`, `samples[].groundTruthS3Object` and
/// `outputConfiguration.s3Object`), all targeting the one `S3Object` shape.
#[derive(Clone, Copy, Debug)]
enum Slot {
    /// `samples[0].assetS3Object.s3Uri`.
    FirstAsset,
    /// `samples[1].groundTruthS3Object.s3Uri` — a second element of the list, and the other
    /// member of a sample.
    SecondGroundTruth,
    /// `outputConfiguration.s3Object.s3Uri` — the place the job writes, so the bucket is
    /// what has to be there.
    Output,
}

/// Every S3 member of the optimization submission, so each rule is asserted of each.
const SLOTS: [Slot; 3] = [Slot::FirstAsset, Slot::SecondGroundTruth, Slot::Output];

impl Slot {
    /// Whether a submission naming `location` in this member is accepted: a member that
    /// names an object the job reads needs the object, the output member needs the bucket.
    fn admits(self, location: &Location) -> bool {
        match self {
            Self::FirstAsset | Self::SecondGroundTruth => location.object_is_held,
            Self::Output => location.location_is_held,
        }
    }
}

/// An `InvokeBlueprintOptimizationAsync` body whose `slot` carries `uri` and whose every
/// other S3 location is a staged object. Two samples, so a row can break a location in
/// either element of the list.
fn optimization_body(blueprint_arn: &str, slot: Slot, uri: &str) -> Value {
    let mut samples = vec![
        json!({
            "assetS3Object": { "s3Uri": format!("s3://{SAMPLE_BUCKET}/{}", STAGED_KEYS[0]) },
            "groundTruthS3Object": { "s3Uri": format!("s3://{SAMPLE_BUCKET}/{}", STAGED_KEYS[1]) },
        }),
        json!({
            "assetS3Object": { "s3Uri": format!("s3://{SAMPLE_BUCKET}/{}", STAGED_KEYS[2]) },
            "groundTruthS3Object": { "s3Uri": format!("s3://{SAMPLE_BUCKET}/{}", STAGED_KEYS[3]) },
        }),
    ];
    let mut output = json!({ "s3Object": { "s3Uri": format!("s3://{OUTPUT_BUCKET}/optimized.json") } });
    match slot {
        Slot::FirstAsset => samples[0]["assetS3Object"]["s3Uri"] = json!(uri),
        Slot::SecondGroundTruth => samples[1]["groundTruthS3Object"]["s3Uri"] = json!(uri),
        Slot::Output => output["s3Object"]["s3Uri"] = json!(uri),
    }
    json!({
        "blueprint": { "blueprintArn": blueprint_arn },
        "samples": samples,
        "outputConfiguration": output,
        "dataAutomationProfileArn": profile_arn(),
    })
}

/// `InvokeBlueprintOptimizationAsync`, per its modelled `@http`
/// `{method: POST, uri: /invokeBlueprintOptimizationAsync, code: 200}`.
async fn invoke_optimization(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/invokeBlueprintOptimizationAsync", body).await
}

/// RULE, on `InvokeBlueprintOptimizationAsync`: a submission is accepted exactly when the
/// location each of its S3 members names is one the object store holds — the object, for a
/// member that names one the job reads; the bucket, for the one it writes into — and is
/// otherwise refused `400 ValidationException` with this service's fixed sentence, minting
/// no job. The accepted ones, and only they, read back through
/// `GetBlueprintOptimizationStatus`.
#[tokio::test]
async fn an_optimization_submission_is_accepted_only_for_locations_the_object_store_holds() {
    let (router, engine) = emulator();
    let blueprint_arn = create_blueprint(&router, "optimize-bp-s3-locations").await;

    let mut accepted = 0usize;
    for slot in SLOTS {
        for location in locations() {
            let case = format!("{slot:?} naming {}", location.case);
            let body = optimization_body(&blueprint_arn, slot, &location.uri);
            let (status, payload) = invoke_optimization(&router, &body).await;
            if slot.admits(&location) {
                accepted += 1;
                assert_eq!(
                    status, 200,
                    "case {case:?}: {} is a location this member needs and the store holds, \
                     so expected 200, got {status} with {payload:?}",
                    location.uri
                );
                // The accepted job is really there: the read resolves the ARN the
                // submission answered.
                let invocation_arn = string_at(&payload, "invocationArn", &case);
                let read_uri = format!(
                    "/getBlueprintOptimizationStatus/{}",
                    encode_label(&invocation_arn)
                );
                let (status, reported) = call(&router, "POST", &read_uri, &json!({})).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the accepted job must read back, got {status} with \
                     {reported:?}"
                );
            } else {
                assert_eq!(
                    status, 400,
                    "case {case:?}: {} is not a location this member can use, so expected \
                     400, got {status} with {payload:?}",
                    location.uri
                );
                assert_eq!(
                    error_code(&payload),
                    "ValidationException",
                    "case {case:?}: expected ValidationException, got {payload:?}"
                );
                assert_eq!(
                    payload.get("message").and_then(Value::as_str),
                    Some(INVALID_S3_URI),
                    "case {case:?}: expected the message {INVALID_S3_URI:?}, got {payload:?}"
                );
            }
        }
    }

    // A refused submission left nothing behind: the store holds exactly the accepted ones.
    let store = engine.state::<ServiceState>();
    let held = store.lock();
    assert_eq!(
        held.optimization_jobs.len(),
        accepted,
        "the store must hold exactly the accepted submissions ({accepted}), got {}",
        held.optimization_jobs.len()
    );
}

// Note on the OTHER operation of this service that carries S3 locations: whether
// `InvokeDataAutomationLibraryIngestionJob`'s locations are ones the object store holds is
// decided by `state::ingestion_locations` and pinned by its own suite, with the sentence
// recorded for THAT operation's refusal; nothing here restates it.

/// RULE: the refusal is GROUNDED in what the object store answered. With no Amazon S3 mounted
/// to ask, nothing is established about a location that addresses one, so a submission naming
/// it is accepted — while a URI that addresses no location at all needs no answer: there is
/// nothing for any store to hold, so it is refused whether a store is there or not.
#[tokio::test]
async fn with_no_object_store_to_ask_only_a_uri_addressing_nothing_is_refused() {
    let (router, engine) = emulator_without_the_object_store();
    let blueprint_arn = create_blueprint(&router, "optimize-bp-s3-ungrounded").await;

    let mut accepted = 0usize;
    for location in locations() {
        let case = format!("FirstAsset naming {} with no object store", location.case);
        let body = optimization_body(&blueprint_arn, Slot::FirstAsset, &location.uri);
        let (status, payload) = invoke_optimization(&router, &body).await;
        if location.addresses_an_object {
            accepted += 1;
            assert_eq!(
                status, 200,
                "case {case:?}: nothing is established about {} with no store to ask, so \
                 expected 200, got {status} with {payload:?}",
                location.uri
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: {} addresses no object for any store to hold, so expected \
                 400, got {status} with {payload:?}",
                location.uri
            );
            assert_eq!(
                payload.get("message").and_then(Value::as_str),
                Some(INVALID_S3_URI),
                "case {case:?}: expected the message {INVALID_S3_URI:?}, got {payload:?}"
            );
        }
    }

    let store = engine.state::<ServiceState>();
    let held = store.lock();
    assert_eq!(
        held.optimization_jobs.len(),
        accepted,
        "the store must hold exactly the accepted submissions ({accepted}), got {}",
        held.optimization_jobs.len()
    );
}

/// The URIs OUTSIDE the modelled `S3Uri` pattern: the scheme, the bucket's character set, its
/// edge characters, and each published length bound with one character past it.
fn uris_outside_the_modelled_pattern() -> Vec<(&'static str, String)> {
    vec![
        ("no scheme at all", "bda-samples/assets/invoice.pdf".to_string()),
        (
            "an http scheme instead of s3",
            "https://bda-samples.s3.amazonaws.com/assets/invoice.pdf".to_string(),
        ),
        ("the s3 scheme with one slash", "s3:/bda-samples/assets/invoice.pdf".to_string()),
        ("an empty authority", "s3:///assets/invoice.pdf".to_string()),
        ("an upper-case bucket name", "s3://BDA-Samples/assets/invoice.pdf".to_string()),
        ("an underscore in the bucket name", "s3://bda_samples/assets/invoice.pdf".to_string()),
        ("a space in the bucket name", "s3://bda samples/assets/invoice.pdf".to_string()),
        ("a two-character bucket name, one under the floor", "s3://ab/key".to_string()),
        (
            "a 64-character bucket name, one over the ceiling",
            format!("s3://{}/key", "b".repeat(64)),
        ),
        ("a bucket name starting with a dash", "s3://-bda-samples/key".to_string()),
        ("a bucket name ending with a dash", "s3://bda-samples-/key".to_string()),
        ("a bucket name ending with a period", "s3://bda.samples./key".to_string()),
    ]
}

/// RULE: a value outside the modelled `S3Uri` `@pattern` is refused by the request boundary
/// itself — `400 ValidationException` naming the member path and the pattern, whichever S3
/// member carries it — and mints no job either.
#[tokio::test]
async fn an_s3_location_outside_the_modelled_pattern_is_refused_by_the_boundary() {
    let (router, engine) = emulator();
    let blueprint_arn = create_blueprint(&router, "optimize-bp-s3-shape").await;

    for slot in SLOTS {
        for (form, uri) in uris_outside_the_modelled_pattern() {
            let case = format!("{slot:?} carrying {form}");
            let body = optimization_body(&blueprint_arn, slot, &uri);
            let (status, payload) = invoke_optimization(&router, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: {uri:?} is outside the modelled S3Uri pattern, so expected \
                 400, got {status} with {payload:?}"
            );
            assert_eq!(
                error_code(&payload),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {payload:?}"
            );
            let message = string_at(&payload, "message", &case);
            assert!(
                message.contains("failed to satisfy constraint")
                    && message.contains("s3://[a-z0-9]"),
                "case {case:?}: expected the boundary's constraint sentence for the modelled \
                 S3Uri pattern, got {message:?}"
            );
        }
    }

    let store = engine.state::<ServiceState>();
    let held = store.lock();
    assert!(
        held.optimization_jobs.is_empty(),
        "a refused submission must leave no job behind, got {} job(s)",
        held.optimization_jobs.len()
    );
}

/// RULE: the walk is BOUNDED — at most
/// `state::s3_location::MAX_RESOLVED_SUBMISSION_LOCATIONS` DISTINCT sample locations of one
/// submission are resolved at all, in the order the submission names them. Inside that
/// bound every location the store does not hold is found, so the submission is refused;
/// past it a location is one nothing is established about, so the submission is not refused
/// on it.
#[tokio::test]
async fn at_most_the_bounded_number_of_locations_is_resolved() {
    let limit = state::s3_location::MAX_RESOLVED_SUBMISSION_LOCATIONS;
    let (router, engine) = emulator();
    // One pair the store does NOT hold, named LAST by every submission below.
    let unheld_pair = json!({
        "assetS3Object": { "s3Uri": format!("s3://{SAMPLE_BUCKET}/never-staged/asset.pdf") },
        "groundTruthS3Object": {
            "s3Uri": format!("s3://{SAMPLE_BUCKET}/never-staged/truth.json"),
        },
    });
    let output = json!({ "s3Object": { "s3Uri": format!("s3://{OUTPUT_BUCKET}/bulk.json") } });
    stage_bulk_samples(&engine, limit / 2);
    let blueprint_arn = create_blueprint(&router, "optimize-bp-s3-bounded").await;

    // (case, how many held pairs the submission names BEFORE the unheld one, accepted?)
    let rows: [(&str, usize, bool); 2] = [
        (
            "the unheld pair past the bound: the held locations named first fill it",
            limit / 2,
            true,
        ),
        (
            "the unheld pair inside the bound: one pair short of filling it",
            limit / 2 - 1,
            false,
        ),
    ];

    let mut accepted = 0usize;
    for (case, held_pairs, is_accepted) in rows {
        let named = held_pairs * 2 + 2;
        let mut samples: Vec<Value> = (0..held_pairs).map(bulk_sample).collect();
        samples.push(unheld_pair.clone());
        let body = json!({
            "blueprint": { "blueprintArn": blueprint_arn },
            "samples": samples,
            "outputConfiguration": output,
            "dataAutomationProfileArn": profile_arn(),
        });
        let (status, payload) = invoke_optimization(&router, &body).await;
        if is_accepted {
            accepted += 1;
            assert_eq!(
                status, 200,
                "case {case:?}: with {named} distinct locations named and a bound of {limit}, \
                 the unheld one establishes nothing, so expected 200, got {status} with \
                 {payload:?}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: with {named} distinct locations named and a bound of {limit}, \
                 the unheld one is resolved, so expected 400, got {status} with {payload:?}"
            );
            assert_eq!(
                payload.get("message").and_then(Value::as_str),
                Some(INVALID_S3_URI),
                "case {case:?}: expected the message {INVALID_S3_URI:?}, got {payload:?}"
            );
        }
    }

    let store = engine.state::<ServiceState>();
    let held = store.lock();
    assert_eq!(
        held.optimization_jobs.len(),
        accepted,
        "the store must hold exactly the accepted submissions ({accepted}), got {}",
        held.optimization_jobs.len()
    );
}
