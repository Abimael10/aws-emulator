// SPDX-License-Identifier: CC0-1.0

//! Which `S3Url` a `CreateOtaTask` is admitted with, and how the OTA-task READ states that
//! the task a request named does not exist.
//!
//! RULE 1 (the artifact URL). `CreateOtaTask`'s `S3Url` is "the URL to the Amazon S3 bucket
//! where the over-the-air (OTA) task is stored"
//! (`smithy_query(query="doc",shape="CreateOtaTaskRequest",member="S3Url")`), so a value
//! that addresses no Amazon S3 OBJECT describes a rollout with nothing to deploy and is
//! refused with the modelled `ValidationException` (HTTP 400, wire code
//! `ValidationException`) carrying AWS's sentence for an S3 fault of the request — `S3 error
//! occurred due to invalid request: ` — and nothing is persisted. The refusal is reached on
//! what the REQUEST carries, so it is stated BEFORE the `Target` and `TaskConfigurationId`
//! references are resolved against the store: a request carrying both an unusable `S3Url`
//! and a reference this region holds nothing under is refused on the URL, not on the
//! reference. Every spelling Amazon S3 publishes for an object is admitted — the `s3://`
//! URI, a virtual-hosted endpoint URL and a path-style one — and an admitted create stores
//! the URL as the request wrote it.
//!
//! RULE 2 (the read's 404). `GetOtaTask` (`GET /ota-tasks/{Identifier}`, `@readonly`) answers
//! an `Identifier` this region holds no task under with the modelled
//! `ResourceNotFoundException` (HTTP 404, wire code `ResourceNotFoundException`) whose
//! `Message` is the sentence this service's OTA-task backend states such an absence with —
//! `Ota task with ID <the Identifier sent> does not exists.` — with the identifier the only
//! part that varies and neither optional member of the error shape filled. The sentence is
//! the same whatever made the task absent: never minted, or held only by another region.
//!
//! Each rule is checked over a TABLE of inputs rather than over one request, and every case
//! drives the real restJson1 request path (`MultiServiceRouter::dispatch`), so the statuses,
//! headers and bodies asserted here are the ones a client sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the suite's requests are signed for unless a case names another.
const REGION: &str = "us-east-1";

/// The route the OTA-task collection is bound to
/// (`smithy_query(query="operation",operation="CreateOtaTask,ListOtaTasks")` ->
/// `POST /ota-tasks`, `GET /ota-tasks`); one task hangs off it under its id.
const COLLECTION: &str = "/ota-tasks";

/// The wire error codes the model's `@httpError` shapes are rendered under.
const VALIDATION: &str = "ValidationException";
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The whole sentence a create whose `S3Url` addresses no object is refused with, written
/// out here rather than imported so the suite pins the text a client reads and not the
/// implementation's own formatter. Its fixed part ends at the colon and the space after it;
/// the detail that would follow is the one the fault carried, and a value refused on its own
/// form carries none.
const UNUSABLE_URL_REASON: &str = "S3 error occurred due to invalid request: ";

/// The fixed halves of the sentence the OTA-task read states, around the identifier the
/// request carried.
const READ_REASON_PREFIX: &str = "Ota task with ID ";
const READ_REASON_SUFFIX: &str = " does not exists.";

/// One HTTP answer, reduced to what the assertions read.
struct Reply {
    status: u16,
    /// The restJson1 error code, from `x-amzn-errortype`.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    fn member(&self, name: &str) -> Option<&Value> {
        self.body.get(name).filter(|value| !value.is_null())
    }

    fn string(&self, name: &str) -> Option<&str> {
        self.member(name).and_then(Value::as_str)
    }

    /// The `Tasks` list of a `ListOtaTasks` answer, which the operation always reports.
    fn tasks(&self) -> Vec<Value> {
        self.member("Tasks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("expected a Tasks list, got body {:?}", self.body))
    }
}

/// A router holding one `iotmanagedintegrations` service over a fresh shared engine: the
/// same assembly `core/router` performs, with one service mounted.
fn emulator() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // The signature itself is not under test here, so requests are unauthenticated.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::new(engine::InMemoryEngine::new())));
    router
}

/// Send one request, signed for `region`, and reduce the answer. The `Authorization`
/// credential scope is what routes the request to this service and names the region its
/// records belong to.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path_and_query: &str,
    body: Option<Value>,
) -> Reply {
    let payload = body.map_or_else(bytes::Bytes::new, |value| {
        bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize the request body"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://iotmanagedintegrations.{region}.amazonaws.com{path_and_query}"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTESTTEST/20260101/{region}/\
                 iotmanagedintegrations/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply {
        status,
        error_type,
        body,
    }
}

/// [`call_in`] in [`REGION`].
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path_and_query: &str,
    body: Option<Value>,
) -> Reply {
    call_in(router, REGION, method, path_and_query, body).await
}

/// The `S3Url` values that address no Amazon S3 object, each with the case it stands for.
///
/// The rows differ along what the rule generalises over: the scheme (none, `s3`, `https`,
/// another one), what the authority is (a name Amazon S3 holds a bucket under, each class of
/// name its published rules refuse, a host that is no S3 endpoint at all), and whether
/// anything follows the bucket. Every value is inside `S3Url`'s modelled `@length` of
/// 1..1000, so each one reaches the handler rather than the constraint walk.
const ADDRESSES_NO_OBJECT: &[(&str, &str)] = &[
    ("no URL at all", "firmware-1.0.bin"),
    ("a bare bucket and key with no scheme", "fleet-updates/firmware.bin"),
    ("the shortest value the member's length floor admits", "s"),
    ("another scheme entirely", "ftp://fleet-updates/firmware.bin"),
    ("an s3 URI naming a bucket and nothing under it", "s3://fleet-updates"),
    ("an s3 URI whose key is a bare separator", "s3://fleet-updates/"),
    ("an s3 URI with no authority", "s3:///firmware.bin"),
    // The bucket-name classes below are the published ones S3 holds no bucket under
    // (`service_common::s3_bucket_name`): upper case, under the three-character floor, an
    // IP-address form, and adjacent periods.
    ("an s3 URI whose bucket is upper case", "s3://Fleet-Updates/firmware.bin"),
    ("an s3 URI whose bucket is under the length floor", "s3://ab/firmware.bin"),
    ("an s3 URI whose bucket is an IP address", "s3://192.168.0.1/firmware.bin"),
    ("an s3 URI whose bucket has adjacent periods", "s3://fleet..updates/firmware.bin"),
    (
        "an https URL whose host is no Amazon S3 endpoint",
        "https://fleet-updates.example.com/firmware.bin",
    ),
    (
        "a virtual-hosted endpoint URL naming the bucket alone",
        "https://fleet-updates.s3.us-east-1.amazonaws.com/",
    ),
    (
        "a path-style endpoint URL naming the bucket alone",
        "https://s3.us-east-1.amazonaws.com/fleet-updates",
    ),
];

/// The `S3Url` values that DO address an object, in each spelling Amazon S3 publishes for
/// one (`service_common::s3_uri`): the `s3://` URI, a virtual-hosted endpoint URL in both
/// Region spellings and on the global endpoint, and a path-style URL — plus keys that are
/// nested, that carry characters a URL reader would read as something else, and one at the
/// member's published `@length` maximum.
fn addresses_an_object() -> Vec<(String, String)> {
    let prefix = "s3://fleet-updates/";
    let at_the_length_maximum = format!(
        "{prefix}{}",
        // `S3Url` is len 1..1000 (`smithy_query(query="shape",shape="S3Url")`).
        "a".repeat(1_000 - prefix.len())
    );
    [
        ("an s3 URI", "s3://fleet-updates/firmware-1.0.bin".to_string()),
        (
            "an s3 URI with a nested key",
            "s3://fleet-updates/lamp/2026/firmware-1.0.bin".to_string(),
        ),
        (
            "an s3 URI whose key carries a question mark, which is part of the key",
            "s3://fleet-updates/firmware?draft=1.bin".to_string(),
        ),
        (
            "the shortest URL that addresses an object",
            "s3://aaa/a".to_string(),
        ),
        ("an s3 URI at the published length maximum", at_the_length_maximum),
        (
            "a virtual-hosted endpoint URL",
            "https://fleet-updates.s3.us-east-1.amazonaws.com/firmware-1.0.bin".to_string(),
        ),
        (
            "a virtual-hosted URL on the global endpoint",
            "https://fleet-updates.s3.amazonaws.com/firmware-1.0.bin".to_string(),
        ),
        (
            "a virtual-hosted URL spelling the Region the legacy way",
            "https://fleet-updates.s3-us-east-1.amazonaws.com/firmware-1.0.bin".to_string(),
        ),
        (
            "a path-style endpoint URL",
            "https://s3.us-east-1.amazonaws.com/fleet-updates/firmware-1.0.bin".to_string(),
        ),
    ]
    .into_iter()
    .map(|(case, url)| (case.to_string(), url))
    .collect()
}

/// An `OtaTaskId`-shaped identifier (`^[A-Za-z0-9]+$`) this region holds no managed thing
/// and no OTA task configuration under, so a create carrying it as a reference would be
/// refused on that reference once the store is reached.
const UNHELD_REFERENCE: &str = "nothingisheldunderthisid0001";

/// RULE 1, the refusal half: a create whose `S3Url` addresses no Amazon S3 object is refused
/// HTTP 400 `ValidationException` with AWS's sentence, persists nothing, and states that
/// refusal even when the same request's references are ones this region holds nothing
/// under — the URL is judged before the store is asked about anything.
#[tokio::test]
async fn a_create_whose_s3_url_addresses_no_object_is_refused_before_any_reference() {
    let router = emulator();

    for (case, url) in ADDRESSES_NO_OBJECT {
        // Each row is sent twice: once carrying only the required members, and once
        // carrying references this region holds nothing under, which the handler would
        // otherwise refuse with its own 404.
        let requests = [
            (
                format!("{case} (the required members alone)"),
                json!({"S3Url": url, "OtaType": "ONE_TIME"}),
            ),
            (
                format!("{case} (beside references nothing holds)"),
                json!({
                    "S3Url": url,
                    "OtaType": "CONTINUOUS",
                    "Target": [UNHELD_REFERENCE],
                    "TaskConfigurationId": UNHELD_REFERENCE,
                }),
            ),
        ];
        for (case, request) in requests {
            let reply = call(&router, "POST", COLLECTION, Some(request)).await;
            assert_eq!(
                reply.status, 400,
                "case {case:?}: expected HTTP 400 for an S3Url that addresses no object, got \
                 {} with body {:?}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error_type.as_deref(),
                Some(VALIDATION),
                "case {case:?}: expected the modelled {VALIDATION} code, got {:?}",
                reply.error_type
            );
            assert_eq!(
                reply.string("Message"),
                Some(UNUSABLE_URL_REASON),
                "case {case:?}: expected the S3 fault sentence, got {:?}",
                reply.member("Message")
            );
        }
    }

    let list = call(&router, "GET", COLLECTION, None).await;
    assert!(
        list.tasks().is_empty(),
        "a refused create must persist nothing, got {:?}",
        list.body
    );
}

/// RULE 1, the admission half: every spelling Amazon S3 publishes for an object is accepted,
/// the create answers HTTP 201, and the read reports the URL exactly as the request wrote
/// it.
#[tokio::test]
async fn a_create_is_admitted_for_every_spelling_that_addresses_an_object() {
    let router = emulator();

    for (case, url) in addresses_an_object() {
        let reply = call(
            &router,
            "POST",
            COLLECTION,
            Some(json!({"S3Url": url, "OtaType": "ONE_TIME"})),
        )
        .await;
        assert_eq!(
            reply.status, 201,
            "case {case:?}: expected HTTP 201 for an S3Url that addresses an object, got {} \
             with body {:?}",
            reply.status, reply.body
        );
        let id = reply
            .string("TaskId")
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a TaskId, got body {:?}", reply.body)
            })
            .to_string();

        let read = call(&router, "GET", &format!("{COLLECTION}/{id}"), None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected HTTP 200 from GetOtaTask, got {} with body {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.string("S3Url"),
            Some(url.as_str()),
            "case {case:?}: the read must report the URL the create carried, got {:?}",
            read.member("S3Url")
        );
    }
}

/// RULE 2: the OTA-task read's 404 states the sentence this service's backend words such an
/// absence with, carrying the identifier the request sent and filling neither optional
/// member of the error shape — whatever made the task absent.
#[tokio::test]
async fn the_read_of_an_absent_task_states_the_services_own_sentence() {
    let router = emulator();

    // A task of ANOTHER region, so one row's absence is a record held elsewhere rather than
    // one nothing ever minted.
    let elsewhere = call_in(
        &router,
        "us-west-2",
        "POST",
        COLLECTION,
        Some(json!({"S3Url": "s3://fleet-updates/firmware-1.0.bin", "OtaType": "ONE_TIME"})),
    )
    .await;
    assert_eq!(
        elsewhere.status, 201,
        "seeding another region's task: expected HTTP 201, got {} with body {:?}",
        elsewhere.status, elsewhere.body
    );
    let held_elsewhere = elsewhere
        .string("TaskId")
        .expect("the seeded create answers with a TaskId")
        .to_string();

    // The rows differ along what the rule generalises over: the form of the identifier —
    // both ends of `OtaTaskId`'s `@length` (1..200) and each character class its `@pattern`
    // (`^[A-Za-z0-9]+$`) admits — and what made the task absent.
    let cases: Vec<(&str, String)> = vec![
        ("one character", "a".to_string()),
        ("two hundred characters, the published maximum", "b".repeat(200)),
        ("digits only", "0123456789".to_string()),
        ("mixed case and digits", "OtaTask42".to_string()),
        ("a task only another region holds", held_elsewhere),
    ];

    for (case, identifier) in &cases {
        let reply = call(&router, "GET", &format!("{COLLECTION}/{identifier}"), None).await;
        assert_eq!(
            reply.status, 404,
            "case {case:?}: expected HTTP 404 for an identifier this region holds no task \
             under, got {} with body {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some(RESOURCE_NOT_FOUND),
            "case {case:?}: expected the modelled {RESOURCE_NOT_FOUND} code, got {:?}",
            reply.error_type
        );
        let expected = format!("{READ_REASON_PREFIX}{identifier}{READ_REASON_SUFFIX}");
        assert_eq!(
            reply.string("Message"),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {:?}",
            reply.member("Message")
        );
        for member in ["ResourceId", "ResourceType"] {
            assert_eq!(
                reply.member(member),
                None,
                "case {case:?}: expected the 404 to carry no {member}, got {:?} in body {:?}",
                reply.member(member),
                reply.body
            );
        }
    }
}
