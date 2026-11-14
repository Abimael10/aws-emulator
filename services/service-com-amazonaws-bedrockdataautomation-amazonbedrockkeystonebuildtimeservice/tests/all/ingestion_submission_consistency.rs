// SPDX-License-Identifier: CC0-1.0

//! Whether an `InvokeDataAutomationLibraryIngestionJob` submission is consistent with what
//! the account's object store holds: its Amazon S3 locations resolved against the
//! emulator's OWN object storage.
//!
//! RULE. A submission carries Amazon S3 locations — the prefix its results go under
//! (`outputConfiguration.s3Uri`, modelled `@required`) and, where it reads its entities
//! from an object rather than from an inline payload, that object
//! (`inputConfiguration.s3Object.s3Uri`). The modelled `S3Uri`
//! (`smithy_query(query="shape",shape="S3Uri")`) constrains only the FORM of such a value,
//! so whether it addresses anything is a question the object store answers: a submission
//! naming a bucket the account's object store holds is accepted and mints its job, and one
//! naming a bucket that store reports it does not hold is refused `400`
//! `ValidationException` with this service's fixed sentence for an invalid input parameter,
//! before anything is minted — so it leaves no job behind and no idempotency token either.
//! The refusal has to be GROUNDED in that answer: an emulator with no object store to ask
//! establishes nothing about the bucket, so the submission is admitted.
//!
//! So this suite mounts the object store beside this service on ONE engine — what the
//! assembled emulator does — and creates its buckets through S3's own front door, so what
//! the refusal is grounded in is S3's answer rather than anything this test seeded. Its
//! tables' rows differ along WHICH location names the absent bucket, the form of the
//! location (a bucket root, a prefix, an object key), the bucket names themselves and the
//! input source the submission reads from.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::state::library_ingestion::{
    INVALID_INGESTION_INPUT_MESSAGE, MISSING_INGESTION_INPUT_MESSAGE,
};
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{encode_label, error_code, string_at};

/// The region every case signs for. `us-east-1`, so `CreateBucket` carries no location
/// constraint (S3 refuses one naming that Region).
const REGION: &str = "us-east-1";

/// An emulator holding this service, with the object store beside it when a case needs one.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    /// This service AND the object store over one shared engine, which is how the assembled
    /// emulator mounts them.
    fn with_object_store() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router }
    }

    /// This service ALONE: nothing answers a question about a bucket.
    fn without_object_store() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// One request against this service's restJson1 wire.
    async fn call(&self, method: &str, uri: &str, body: &Value) -> Reply {
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "bedrock-data-automation.amazonaws.com")
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTKEY/20240101/{REGION}/bedrock/\
                     aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("serializable body"),
            ))
            .expect("a valid test request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(|value| error_code(&json!({ "__type": value })));
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

    /// `PUT /<bucket>` against the emulator's S3 front door — `CreateBucket` in the Region
    /// every case signs for, which takes no location constraint.
    async fn create_bucket(&self, bucket: &str) -> u16 {
        let request = http::Request::builder()
            .method("PUT")
            .uri(format!("https://s3.{REGION}.amazonaws.com/{bucket}"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTKEY/20240101/{REGION}/s3/\
                     aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(Bytes::new())
            .expect("a valid S3 request");
        self.router.dispatch(request).await.status().as_u16()
    }

    /// Create a bucket, requiring that the object store holds it afterwards.
    async fn bucket(&self, bucket: &str) -> String {
        let status = self.create_bucket(bucket).await;
        assert_eq!(
            status, 200,
            "bucket {bucket:?}: expected CreateBucket 200, got {status}"
        );
        bucket.to_string()
    }

    /// `CreateDataAutomationLibrary`; returns the `libraryArn` it minted.
    async fn library(&self, name: &str) -> String {
        let reply = self
            .call(
                "PUT",
                "/data-automation-libraries/",
                &json!({ "libraryName": name }),
            )
            .await;
        assert_eq!(
            reply.status, 201,
            "case {name:?}: expected CreateDataAutomationLibrary 201, got {} with {:?}",
            reply.status, reply.body
        );
        string_at(&reply.body, "libraryArn", name)
    }

    /// `InvokeDataAutomationLibraryIngestionJob`.
    async fn invoke(&self, library_arn: &str, body: &Value) -> Reply {
        self.call("PUT", &jobs_uri(library_arn), body).await
    }

    /// The ARNs `ListDataAutomationLibraryIngestionJobs` enumerates for one library.
    async fn listed_jobs(&self, library_arn: &str, case: &str) -> Vec<String> {
        let reply = self.call("POST", &jobs_uri(library_arn), &json!({})).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected ListDataAutomationLibraryIngestionJobs 200, got {} with \
             {:?}",
            reply.status, reply.body
        );
        reply
            .body
            .get("jobs")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: no jobs list in {:?}", reply.body))
            .iter()
            .map(|item| string_at(item, "jobArn", case))
            .collect()
    }
}

/// What one call answered: its status, its `x-amzn-errortype` and the decoded body.
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

/// The job collection's path for one library, as the modelled `@http` URIs bind it
/// (`/data-automation-libraries/{libraryArn}/library-ingestion-jobs/`).
fn jobs_uri(library_arn: &str) -> String {
    format!(
        "/data-automation-libraries/{}/library-ingestion-jobs/",
        encode_label(library_arn)
    )
}

/// An `inputConfiguration` carrying the modelled `InlinePayload` union's upsert arm, which
/// names no Amazon S3 location at all.
fn inline_upsert_input() -> Value {
    json!({
        "inlinePayload": {
            "upsertEntitiesInfo": [{
                "vocabulary": {
                    "entityId": "acme-term",
                    "language": "EN",
                    "phrases": [{ "text": "acme" }],
                }
            }]
        }
    })
}

/// An `inputConfiguration` carrying the union's delete arm, which names no location either.
fn inline_delete_input() -> Value {
    json!({ "inlinePayload": { "deleteEntitiesInfo": { "entityIds": ["acme-term"] } } })
}

/// An `inputConfiguration` reading the object `s3_uri` addresses.
fn object_input(s3_uri: &str) -> Value {
    json!({ "s3Object": { "s3Uri": s3_uri } })
}

/// A submission body: the four required members, with the input source and the output
/// location the case names.
fn submission(input: Value, output_s3_uri: &str) -> Value {
    json!({
        "entityType": "VOCABULARY",
        "operationType": "UPSERT",
        "inputConfiguration": input,
        "outputConfiguration": { "s3Uri": output_s3_uri },
    })
}

/// Require one submission to be refused as carrying an invalid input parameter, and to have
/// minted nothing.
fn assert_invalid_input(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected HTTP 400, got {} with {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        error_code(&reply.body),
        "ValidationException",
        "case {case:?}: expected a ValidationException, got {:?}",
        reply.body
    );
    assert_eq!(
        reply.error_type.as_deref(),
        Some("ValidationException"),
        "case {case:?}: expected the error type on the wire, got {:?}",
        reply.error_type
    );
    assert_eq!(
        reply.body.get("message").and_then(Value::as_str),
        Some(INVALID_INGESTION_INPUT_MESSAGE),
        "case {case:?}: expected {INVALID_INGESTION_INPUT_MESSAGE:?}, got {:?}",
        reply.body
    );
    assert!(
        reply.body.get("jobArn").is_none(),
        "case {case:?}: a refusal answers no jobArn, got {:?}",
        reply.body
    );
}

/// RULE: a submission is accepted only when every Amazon S3 location it names is a location
/// of a bucket the object store holds; one naming a bucket that store does not hold is
/// refused as an invalid input parameter and mints nothing.
///
/// The rows differ along WHICH location names the absent bucket (the output prefix, the
/// input object, or neither), the FORM of the location (a bucket root, a prefix, an object
/// key), the input source read from and the bucket names themselves.
#[tokio::test]
async fn a_submission_is_accepted_only_for_locations_the_object_store_holds() {
    let emulator = Emulator::with_object_store();
    // Bucket names differing along what an S3 bucket name may carry — length, hyphens,
    // digits, dots — each created through the object store's own front door, so this
    // service's answer is grounded in S3 holding it.
    let output = emulator.bucket("bda-ingestion-output").await;
    let assets = emulator.bucket("bda.assets-01").await;
    let library = emulator.library("ingestion-location-library").await;

    // (case, the inputConfiguration, the outputConfiguration's s3Uri, accepted?)
    let rows: [(&str, Value, String, bool); 8] = [
        (
            "an inline upsert with its output under a held bucket's prefix",
            inline_upsert_input(),
            format!("s3://{output}/library/output/"),
            true,
        ),
        (
            "an inline delete with its output at a held bucket's root",
            inline_delete_input(),
            format!("s3://{output}"),
            true,
        ),
        (
            "an inline upsert with its output under a bucket the store does not hold",
            inline_upsert_input(),
            "s3://bda-ingestion-output-absent/library/output/".to_string(),
            false,
        ),
        (
            "an inline upsert with its output at the root of a bucket the store does not hold",
            inline_upsert_input(),
            "s3://bda.assets-absent-01".to_string(),
            false,
        ),
        (
            "an object read from a held bucket, with its output under a held bucket",
            object_input(&format!("s3://{assets}/entities/vocabulary.json")),
            format!("s3://{output}/library/output/"),
            true,
        ),
        (
            "an object read from a bucket the store does not hold",
            object_input("s3://bda-assets-absent/entities/vocabulary.json"),
            format!("s3://{output}/library/output/"),
            false,
        ),
        (
            "an object read from a held bucket, with its output under one the store does not \
             hold",
            object_input(&format!("s3://{assets}/entities/vocabulary.json")),
            "s3://bda-ingestion-output-absent/library/output/".to_string(),
            false,
        ),
        (
            "an object read from a held bucket's root-level key",
            object_input(&format!("s3://{assets}/vocabulary.json")),
            format!("s3://{output}/other-output"),
            true,
        ),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (case, input, output_s3_uri, accepted) in rows {
        let reply = emulator
            .invoke(&library, &submission(input, &output_s3_uri))
            .await;
        if accepted {
            assert_eq!(
                reply.status, 201,
                "case {case:?}: expected HTTP 201, got {} with {:?}",
                reply.status, reply.body
            );
            minted.push(string_at(&reply.body, "jobArn", case));
        } else {
            assert_invalid_input(case, &reply);
        }
        // Whatever the answer, the library holds exactly the jobs the ACCEPTED submissions
        // minted: a refused submission leaves nothing behind.
        let mut listed = emulator.listed_jobs(&library, case).await;
        listed.sort();
        let mut expected = minted.clone();
        expected.sort();
        assert_eq!(
            listed, expected,
            "case {case:?}: expected exactly the accepted submissions' jobs, got {listed:?}"
        );
    }
}

/// RULE: the refusal is grounded in what the object store ANSWERED, so an emulator with no
/// object store to ask establishes nothing about a bucket and the submission is admitted.
///
/// The rows differ along the location that cannot be resolved (the output prefix, the input
/// object) and the bucket names named.
#[tokio::test]
async fn nothing_is_refused_when_no_object_store_answers() {
    let emulator = Emulator::without_object_store();
    let library = emulator.library("ingestion-unanswered-library").await;

    // (case, the inputConfiguration, the outputConfiguration's s3Uri)
    let rows: [(&str, Value, &str); 3] = [
        (
            "an inline upsert naming an output bucket nothing can resolve",
            inline_upsert_input(),
            "s3://bda-ingestion-output/library/output/",
        ),
        (
            "an inline delete naming another output bucket",
            inline_delete_input(),
            "s3://bda.assets-01/out",
        ),
        (
            "an object read from a bucket nothing can resolve",
            object_input("s3://bda-assets/entities/vocabulary.json"),
            "s3://bda-ingestion-output/library/output/",
        ),
    ];

    let mut minted = 0usize;
    for (case, input, output_s3_uri) in rows {
        let reply = emulator.invoke(&library, &submission(input, output_s3_uri)).await;
        assert_eq!(
            reply.status, 201,
            "case {case:?}: with nothing to answer about the bucket the submission must be \
             admitted, got {} with {:?}",
            reply.status, reply.body
        );
        minted += 1;
        assert_eq!(
            emulator.listed_jobs(&library, case).await.len(),
            minted,
            "case {case:?}: expected {minted} job(s) under the library"
        );
    }
}

/// RULE: the two refusals of this operation's pre-mint validation stay distinct — a
/// submission naming NO input source is answered with the sentence naming the two sources
/// it could have named, while a location of no bucket is answered with the
/// invalid-input-parameter sentence.
#[tokio::test]
async fn naming_no_input_source_keeps_its_own_sentence() {
    let emulator = Emulator::with_object_store();
    let output = emulator.bucket("bda-sentence-output").await;
    let library = emulator.library("ingestion-sentence-library").await;

    let case = "a submission naming no input source, with a held output bucket";
    let reply = emulator
        .invoke(
            &library,
            &submission(json!({}), &format!("s3://{output}/out/")),
        )
        .await;
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected HTTP 400, got {} with {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.body.get("message").and_then(Value::as_str),
        Some(MISSING_INGESTION_INPUT_MESSAGE),
        "case {case:?}: expected {MISSING_INGESTION_INPUT_MESSAGE:?}, got {:?}",
        reply.body
    );

    let case = "a submission whose output names a bucket the store does not hold";
    let reply = emulator
        .invoke(
            &library,
            &submission(inline_upsert_input(), "s3://bda-sentence-absent/out/"),
        )
        .await;
    assert_invalid_input(case, &reply);
    assert!(
        emulator.listed_jobs(&library, case).await.is_empty(),
        "case {case:?}: neither refusal may mint a job"
    );
}

/// RULE: the location is resolved BEFORE anything is minted, so a refused submission
/// consumes no idempotency token either — the same token later carries an accepted
/// submission and mints its job.
#[tokio::test]
async fn a_refused_submission_consumes_no_idempotency_token() {
    let emulator = Emulator::with_object_store();
    let output = emulator.bucket("bda-token-output").await;
    let library = emulator.library("ingestion-token-library").await;
    // `ClientToken` is modelled @length(33..256), pattern ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$.
    let token = "ingestionlocationtoken00000000001";

    let case = "a submission under a token, naming a bucket the store does not hold";
    let mut body = submission(inline_upsert_input(), "s3://bda-token-absent/out/");
    body["clientToken"] = json!(token);
    let reply = emulator.invoke(&library, &body).await;
    assert_invalid_input(case, &reply);

    let case = "the same token on a submission every location of which resolves";
    let mut body = submission(inline_upsert_input(), &format!("s3://{output}/out/"));
    body["clientToken"] = json!(token);
    let reply = emulator.invoke(&library, &body).await;
    assert_eq!(
        reply.status, 201,
        "case {case:?}: expected HTTP 201, got {} with {:?}",
        reply.status, reply.body
    );
    let job_arn = string_at(&reply.body, "jobArn", case);
    assert_eq!(
        emulator.listed_jobs(&library, case).await,
        vec![job_arn],
        "case {case:?}: exactly the accepted submission's job must be enumerated"
    );
}
