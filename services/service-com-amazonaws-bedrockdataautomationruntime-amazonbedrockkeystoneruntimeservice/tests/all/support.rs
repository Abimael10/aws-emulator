// SPDX-License-Identifier: CC0-1.0

//! Helpers shared by this crate's test suites.
//!
//! A submission names WHAT to extract by reference, and the referenced data-automation
//! project belongs to the build-time service rather than to this one, so a suite whose
//! submissions name a project has to ARRANGE that project in the owning service's store
//! before the submission can be accepted.
//!
//! [`seed_project`] is that arrangement, for a suite that is about something else and
//! names its project by a FIXED ARN (the ARN a create mints is the store's to choose, so
//! a suite that wants a particular one writes the record). The suite that is about the
//! resolution rule itself (`control_plane_reference_resolution`) does not use this: it
//! creates every project and blueprint it references through the build-time service's own
//! `CreateDataAutomationProject` / `CreateBlueprint` over the wire.
//!
//! The second half of this module is the WIRE harness — [`Answer`], [`call`] and the
//! tag-key fixture around them — which the suites about the modelled `TagKey` constraint
//! share instead of each carrying its own copy of the dispatch, the credential scope and
//! the submission body, followed by the AMAZON S3 half of it ([`ObjectAnswer`], [`s3_call`]
//! and the object helpers around them) and the terminal-status poll
//! ([`poll_until_terminal`]): a job's output objects belong to the object store, so the
//! suites about what a finished job wrote read them back over that service's own wire
//! protocol, and they share one harness for it.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::state as build_time;
use service_com_amazonaws_bedrockdataautomationruntime_amazonbedrockkeystoneruntimeservice as bda;
use smithy_server_runtime::MultiServiceRouter;

/// The `invocationId` inside a minted invocation ARN: the segment after the resource
/// prefix the ARN is built around (`state::invocation_arn`).
///
/// A job's output lives under a segment of its OWN id below the submitted output prefix,
/// so the id is what a reader's reported output location is checked against
/// ([`job_metadata_uri`]).
pub fn invocation_id(invocation_arn: &str) -> &str {
    invocation_arn.split('/').next_back().unwrap_or_default()
}

/// Where `GetDataAutomationStatus` reports the output of the job `invocation_arn` names,
/// for a submission whose `outputConfiguration.s3Uri` was `output_prefix`: that job's own
/// `job_metadata.json`, under its id below the prefix.
///
/// For the suites whose subject is something else and which only have to know that the
/// reader answers for the record their submission minted; the rule itself — including how
/// a prefix with and without a trailing separator are joined — is asserted row by row in
/// `job_output_location_and_outcome`.
pub fn job_metadata_uri(output_prefix: &str, invocation_arn: &str) -> String {
    format!(
        "{}/{}/job_metadata.json",
        output_prefix.trim_end_matches('/'),
        invocation_id(invocation_arn)
    )
}

/// Register one data-automation project, under `project_arn` and named `project_name`, in
/// the build-time service's store on `engine` — a project in its settled stage, as a
/// create that has already completed leaves it.
pub fn seed_project(engine: &engine::InMemoryEngine, project_arn: &str, project_name: &str) {
    let state = build_time::service_state(engine).expect("the build-time store on this engine");
    let created = build_time::now();
    let mut store = state.lock();
    store.projects.insert(
        project_arn.to_string(),
        build_time::DataAutomationProjectRecord {
            project_arn: project_arn.to_string(),
            project_name: project_name.to_string(),
            project_stage: build_time::DEFAULT_PROJECT_STAGE.to_string(),
            project_type: "ASYNC".to_string(),
            project_description: None,
            standard_output_configuration: None,
            custom_output_configuration: None,
            override_configuration: None,
            data_automation_library_configuration: None,
            creation_time: created,
            last_modified_time: created,
            kms_key_id: None,
            kms_encryption_context: None,
            completes_at: SystemTime::now(),
            tags: Vec::new(),
        },
    );
    store
        .project_names
        .insert(project_name.to_string(), project_arn.to_string());
}

/// This service's sigv4 signing name (`OPERATIONS.md`): the credential scope the router
/// routes on.
pub const SIGNING_NAME: &str = "bedrock";

/// The region the requests [`call`] builds sign for.
pub const REGION: &str = "us-west-2";

/// A legal `dataAutomationProfileArn` (pattern from `smithy_query` shape
/// `DataAutomationProfileArn`, whose account segment accepts the literal `aws`).
pub const PROFILE_ARN: &str =
    "arn:aws:bedrock:us-west-2:aws:data-automation-profile/us.data-automation-v1";

/// A legal `dataAutomationConfiguration.dataAutomationProjectArn` (pattern from
/// `smithy_query` shape `DataAutomationProjectArn`): what tells the service WHAT to
/// extract, which every accepted submission has to name. [`tag_key_router`] arranges it in
/// the store of the service that owns it.
pub const PROJECT_ARN: &str =
    "arn:aws:bedrock:us-west-2:000000000000:data-automation-project/receipts";

/// The modelled `TagKey` pattern without the outer anchors a Smithy `@pattern` does not
/// need (`smithy_query` shape `TagKey`), as a refusal's requirement clause prints it.
pub const UNANCHORED_TAG_KEY_PATTERN: &str = r"(?!aws:)[\p{L}\p{Z}\p{N}_.:/=+\-@]*";

/// One wire answer: the HTTP status and the parsed JSON body.
pub struct Answer {
    /// The HTTP status the router answered with.
    pub status: u16,
    /// The response body, parsed, or `Value::Null` for an empty one.
    pub body: Value,
}

impl Answer {
    /// The awsJson error code: the body's `__type`, local name only.
    pub fn error_code(&self) -> &str {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .split('#')
            .next_back()
            .unwrap_or_default()
    }

    /// The error message the body carries, or the empty string.
    pub fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

/// Call one operation on `router` the way an SDK would: the awsJson1_1 target header, the
/// JSON body and a credential scope in this service's signing name.
pub async fn call(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "x-amz-target",
            format!("AmazonBedrockKeystoneRuntimeService.{operation}"),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{SIGNING_NAME}/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("serialize the request body"),
        ))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("a JSON response body")
    };
    Answer { status, body }
}

/// A router serving only this service over one shared engine, with [`PROJECT_ARN`]
/// arranged under `project_name` in the store of the service that OWNS data-automation
/// projects: a submission naming a project nobody created is refused before any tag rule
/// can be reached.
pub fn tag_key_router(project_name: &str) -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    seed_project(&engine, PROJECT_ARN, project_name);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(bda::EmulatedService::new(engine));
    router
}

/// A submission carrying every required member at its minimum, writing its output below
/// `output`, optionally with `tags`.
pub fn tag_key_submission(output: &str, tags: Option<Value>) -> Value {
    let mut body = json!({
        "inputConfiguration": {"s3Uri": "s3://in-bucket/assets/receipt.pdf"},
        "outputConfiguration": {"s3Uri": output},
        "dataAutomationProfileArn": PROFILE_ARN,
        "dataAutomationConfiguration": {"dataAutomationProjectArn": PROJECT_ARN},
    });
    if let Some(tags) = tags {
        body["tags"] = tags;
    }
    body
}

/// The tag keys the resource `arn` currently carries, read back through
/// `ListTagsForResource` and sorted.
pub async fn tag_keys(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    let answer = call(router, "ListTagsForResource", json!({"resourceARN": arn})).await;
    assert_eq!(
        answer.status, 200,
        "the resource's tags must read back, got {} / {}",
        answer.status, answer.body
    );
    let mut keys: Vec<String> = answer.body["tags"]
        .as_array()
        .expect("the answer must carry a tags list")
        .iter()
        .map(|tag| {
            tag["key"]
                .as_str()
                .expect("every reported tag has a key")
                .to_string()
        })
        .collect();
    keys.sort();
    keys
}

/// A PDF document of `pages`, each page showing its own lines in the Base-14 Helvetica face
/// every PDF reader carries (nothing is embedded, so the document stands alone).
///
/// Written by the `pdf-writer` crate, so the asset a job reads is a document a PDF parser
/// opens rather than bytes shaped like one. Shared by the suites whose subject is what a job
/// EXTRACTS out of such an asset, so one document builder serves them all.
pub fn pdf_document(pages: &[&[&str]]) -> Vec<u8> {
    use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str};

    /// The ISO 216 A4 page box in PDF points (1/72 inch).
    const A4: [f32; 4] = [0.0, 0.0, 595.28, 841.89];
    /// Point size of the body text, and the baseline-to-baseline distance with it.
    const FONT_SIZE: f32 = 12.0;
    /// See [`FONT_SIZE`].
    const LEADING: f32 = 16.0;

    let catalog_id = Ref::new(1);
    let page_tree_id = Ref::new(2);
    let font_id = Ref::new(3);
    let font_name = Name(b"F1");

    let mut pdf = Pdf::new();
    pdf.catalog(catalog_id).pages(page_tree_id);
    // Two object ids per page: the page and its content stream, after the three above.
    let page_ids: Vec<Ref> = (0..pages.len())
        .map(|index| Ref::new(4 + 2 * i32::try_from(index).expect("a small page count")))
        .collect();
    let content_ids: Vec<Ref> = (0..pages.len())
        .map(|index| Ref::new(5 + 2 * i32::try_from(index).expect("a small page count")))
        .collect();
    pdf.pages(page_tree_id)
        .kids(page_ids.iter().copied())
        .count(i32::try_from(pages.len()).expect("a small page count"));
    for ((lines, page_id), content_id) in pages.iter().zip(&page_ids).zip(&content_ids) {
        let mut content = Content::new();
        content.begin_text();
        content.set_font(font_name, FONT_SIZE);
        content.set_leading(LEADING);
        content.next_line(56.0, 785.0);
        for line in lines.iter() {
            content.show(Str(line.as_bytes()));
            content.next_line(0.0, -LEADING);
        }
        content.end_text();
        let content = content.finish();

        let mut page = pdf.page(*page_id);
        page.parent(page_tree_id);
        page.media_box(Rect::new(A4[0], A4[1], A4[2], A4[3]));
        page.contents(*content_id);
        page.resources().fonts().pair(font_name, font_id);
        page.finish();
        pdf.stream(*content_id, &content);
    }
    pdf.type1_font(font_id).base_font(Name(b"Helvetica"));
    pdf.finish()
}

/// The media type an object written with no `Content-Type` of its own carries (Amazon S3's
/// documented default for an upload that specifies none) — what a client reads a job's
/// output document with.
pub const DEFAULT_MEDIA_TYPE: &str = "binary/octet-stream";

/// The `ChecksumType` a single whole-object write settles: there are no parts to compose a
/// checksum from.
pub const FULL_OBJECT: &str = "FULL_OBJECT";

/// The server-side encryption Amazon S3 applies to a new object by default (SSE-S3).
pub const SSE_S3: &str = "AES256";

/// How long [`poll_until_terminal`] may wait for a job to reach a terminal status.
///
/// GENEROUS on purpose: the emulated lifecycle spans are a fraction of a second, and this is
/// only the deadline that stops a broken build from hanging — the loop waits for the
/// CONDITION, not for a duration, so a loaded machine takes longer and still passes.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one poll sleeps before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One raw Amazon S3 answer: the status, the response headers and the body.
///
/// A job's output objects belong to the OBJECT STORE, so every suite about them reads them
/// back over Amazon S3's own wire protocol rather than out of this service — which is why the
/// harness lives here, shared, instead of once per suite.
pub struct ObjectAnswer {
    /// The HTTP status the object store answered with.
    pub status: u16,
    /// The response headers, which is where a stored object's own metadata travels.
    pub headers: http::HeaderMap,
    /// The response body: the object's bytes, a listing, or a refusal document.
    pub body: Vec<u8>,
}

impl ObjectAnswer {
    /// One response header's value, or `None` when the answer carries no such header.
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string)
    }

    /// The body as text, for an answer a case reports in an assertion message.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The body as JSON, or `Value::Null` when it is not a JSON document.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// One path-style request to the emulator's own Amazon S3 front door: the bucket and the key
/// live in the URI, the body is the object's own bytes. The credential scope names `s3`,
/// which is what claims it.
pub async fn s3_call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> ObjectAnswer {
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://s3.{REGION}.amazonaws.com{path}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        );
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    ObjectAnswer {
        status: response.status().as_u16(),
        headers: response.headers().clone(),
        body: response.body().as_ref().to_vec(),
    }
}

/// Create `bucket` in [`REGION`], the way a caller creates one before staging anything in it.
///
/// A region-specific endpoint needs the location constraint stated: "the unspecified location
/// constraint is incompatible for the region specific endpoint this request was sent to" is
/// what the object store answers a create without one.
pub async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let configuration = format!(
        "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
         </CreateBucketConfiguration>"
    );
    let answer = s3_call(
        router,
        "PUT",
        &format!("/{bucket}"),
        &[],
        configuration.as_bytes(),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "the bucket {bucket:?} must be created, got {} / {}",
        answer.status,
        answer.text()
    );
}

/// Store `bytes` at `key` in `bucket`, the way a caller uploads an input asset before
/// submitting a job that reads it.
pub async fn put_object(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    bytes: &[u8],
) {
    let answer = s3_call(router, "PUT", &format!("/{bucket}/{key}"), &[], bytes).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the object {key:?} must be stored in {bucket:?}, got {} / {}",
        answer.status,
        answer.text()
    );
}

/// Read one object back with the checksum mode enabled, which is what asks Amazon S3 for the
/// checksum it stored ("to retrieve the checksum, this mode must be enabled",
/// `GetObjectRequest$ChecksumMode`).
pub async fn read_object(router: &MultiServiceRouter, bucket: &str, key: &str) -> ObjectAnswer {
    s3_call(
        router,
        "GET",
        &format!("/{bucket}/{key}"),
        &[("x-amz-checksum-mode", "ENABLED")],
        &[],
    )
    .await
}

/// Ask for one object's metadata alone, with the same checksum mode.
pub async fn head_object(router: &MultiServiceRouter, bucket: &str, key: &str) -> ObjectAnswer {
    s3_call(
        router,
        "HEAD",
        &format!("/{bucket}/{key}"),
        &[("x-amz-checksum-mode", "ENABLED")],
        &[],
    )
    .await
}

/// Poll `GetDataAutomationStatus` until the job `invocation_arn` names reaches a terminal
/// status, and answer the FIRST body that reported it — so every assertion a suite makes
/// about that job's output is made against the very read a polling client would have acted
/// on.
///
/// The wait is bounded by [`TERMINAL_DEADLINE`] and is a wait for the CONDITION: the loop
/// asks again until the job reports terminal, so nothing here depends on how fast the host
/// runs.
pub async fn poll_until_terminal(
    router: &MultiServiceRouter,
    case: &str,
    invocation_arn: &str,
) -> Value {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let answer = call(
            router,
            "GetDataAutomationStatus",
            json!({"invocationArn": invocation_arn}),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200 from GetDataAutomationStatus, got {} / {}",
            answer.status, answer.body
        );
        let status = answer.body["status"].as_str().unwrap_or_default();
        if matches!(status, "Success" | "ClientError" | "ServiceError") {
            return answer.body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached a terminal status, last read {}",
            answer.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The refusal message a tag key outside the modelled pattern earns at input path `path`:
/// the `ValidationException` envelope around one clause naming the member and the
/// constraint it failed, with the offending value NOT echoed.
pub fn tag_key_refusal_message(path: &str) -> String {
    format!(
        "1 validation error detected: Value at '{path}' failed to satisfy constraint: \
         Member must satisfy regular expression pattern: {UNANCHORED_TAG_KEY_PATTERN}"
    )
}
