// SPDX-License-Identifier: CC0-1.0

//! An asynchronous PHI-detection job Amazon Comprehend Medical reports `COMPLETED` has
//! ANALYSED the documents its submission named and WRITTEN that analysis into this
//! emulator's own Amazon S3, under a data-access role this emulator's own IAM answered for.
//!
//! RULE: a batch detection job's terminal status reports the work its run actually did.
//! `StartPHIDetectionJob` "starts an asynchronous job to detect protected health
//! information", its `OutputDataConfig` "specifies where to send the output files" and the
//! `ManifestFilePath` of the job it creates is "the path to the file that describes the
//! results of a batch job"
//! (`smithy_query(query="operation",operation="StartPHIDetectionJob")`,
//! `smithy_query(query="shape",shape="ComprehendMedicalAsyncJobProperties")`), so a run
//! that read every document under the submitted `InputDataConfig`, analysed each one and
//! wrote that analysis back under the submitted `OutputDataConfig` reaches `COMPLETED`
//! carrying that path, while the modelled failure state is reserved for a run that
//! genuinely could not be carried out and says why. Three consequences are asserted here:
//!
//! * the analysis written for a document is the SAME protected-health-information pass the
//!   synchronous `DetectPHI` answers with, so a document's output file is the entities of
//!   that document's own text, placed at their own offsets in it;
//! * `ClientRequestToken` is the model's `@idempotencyToken`, so it identifies ONE
//!   submission: repeating that submission names the job it already created and performs
//!   its run once, while the same token carrying different parameters is a submission of
//!   its own, with its own job and its own output, leaving the first job untouched;
//! * a `JobId` the family holds no job for is refused with one fixed sentence that echoes
//!   nothing of the request, while a value outside the member's modelled constraints keeps
//!   its constraint report;
//! * a job RUNS AS the role its submission names, so no run happens until this account's
//!   own IAM has answered that this service may assume that role: a role IAM holds nothing
//!   under, or one whose trust policy admits another service, backs no job at all.
//!
//! This suite lives here rather than in the service's own crate because the rules only hold
//! with THREE services on one engine: the submission's `DataAccessRoleArn` is resolved
//! against IAM's own records before a `JobId` is minted, and the run reads its documents
//! from — and writes its output into — the object store, over the emulator's own in-process
//! callback. That is the shape a client meets on the assembled emulator, and the refusals of
//! the last rule are grounded in what IAM ANSWERED, so nothing but a real IAM beside the
//! service produces them.
//!
//! The rows span what the rules generalise over: how many documents an input holds and how
//! they are keyed, whether the input prefix names a directory or one object, whether an
//! output prefix was given at all, whether the input holds a document to analyse at all,
//! which parameter a reused token changes, the form of an id that names no job, and whether
//! IAM answers for the role a submission names.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form-body encoder and the XML readers this suite needs are the ones
// `all/query_wire.rs` owns for this whole test crate, so neither codec is written again
// here.
use crate::query_wire::{element, elements, form_encode};

/// The Region every call here is signed for, which is the Region the service resolves IAM
/// and the object store in.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix of this service's wire protocol (awsJson1_1, `POST /`).
const TARGET_PREFIX: &str = "ComprehendMedical_20181030";

/// The data-access role every submission passes, created in IAM below with a trust policy
/// that admits this service's own principal.
const ROLE_NAME: &str = "comprehend-medical-batch-role";

/// The service principal a data-access role has to admit for a job to run under it.
const SERVICE_PRINCIPAL: &str = "comprehendmedical.amazonaws.com";

/// A service principal that is NOT this service, for the role no job of this service may
/// run under.
const OTHER_PRINCIPAL: &str = "lambda.amazonaws.com";

/// What a submission is refused with when this account's IAM answers that the role it
/// names cannot be assumed by this service: fixed text, pointing at the trust policy an
/// asynchronous job's role needs rather than at the value the request carried.
const ROLE_NOT_TRUSTED: &str =
    "The provided IAM role does not trust Comprehend Medical service principle. See: \
     https://docs.aws.amazon.com/comprehend-medical/latest/dev/textanalysis-batchapi.html#batch-iam";

/// The statuses the model's `JobStatus` enum calls finished, for the polls below.
const TERMINAL_STATUSES: &[&str] = &["COMPLETED", "PARTIAL_SUCCESS", "FAILED", "STOPPED"];

/// How long a poll waits for a job to finish before the test concludes it is wedged.
/// Deliberately generous: the job's own spans are fractions of a second, and this bound is
/// here so a stuck run fails the test instead of hanging it.
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks at one job.
const POLL_STEP: Duration = Duration::from_millis(50);

/// The `Category` every entity a PHI detection reports carries
/// (`smithy_query(query="shape",shape="EntityType")` lists
/// `PROTECTED_HEALTH_INFORMATION` as the category of this detection's entities).
const PHI_CATEGORY: &str = "PROTECTED_HEALTH_INFORMATION";

/// The `Type` of the entity that covers a person's name.
const NAME_TYPE: &str = "NAME";

/// What the service reports for a request naming a job it cannot find: fixed text, with no
/// variable part at all.
const JOB_NOT_FOUND: &str = "Could not find specified job.";

/// The HTTP status the recorded refusals of a well-formed `JobId` naming no job carry.
///
/// It is NOT the 404 the not-found shape's own `@httpError` declares: the recordings this
/// service's refusal is built from answer the absent-job condition with the status it
/// answers a client-input refusal with, which is why the service states that override in
/// one place (`src/service/job_refusal.rs`) rather than per handler. Asserted here as the
/// recordings carry it, so this suite pins what a client observes rather than one handler.
const JOB_NOT_FOUND_STATUS: u16 = 400;

/// The name of the file describing a finished run's results, inside the run's own output
/// directory, which is what `ManifestFilePath` points at.
const MANIFEST_NAME: &str = "Manifest.json";

/// Amazon S3, IAM and Amazon Comprehend Medical over ONE shared engine, as
/// `router/src/lib.rs` mounts them — so the service's in-process callback reaches this very
/// object store and this very IAM.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_comprehendmedical_comprehendmedical_20181030::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header scoped to `service` in [`REGION`], which is what routes
/// the request and what a service reads the caller's Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One raw request to the emulator's Amazon S3 front door: path-style, so the bucket and
/// key live in the URI and the body is the object's own bytes.
///
/// Written here rather than taken from `all/sts_session_credential_refusals.rs`, whose own
/// `s3` is about which CREDENTIAL presents a request and is scoped to that suite's Region:
/// this one presents the one ambient caller in [`REGION`] and carries a document's own
/// bytes, with the media type an input document is staged under.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .header("content-type", "text/plain")
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One awsQuery call to IAM: `POST /` with the parameters in the form body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One call to Amazon Comprehend Medical over the wire it speaks.
async fn comprehend_medical(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("comprehendmedical.{REGION}.amazonaws.com"))
        .header("authorization", authorization("comprehendmedical"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the test body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `PUT /<bucket>`: the bucket a row's documents are staged in and its output written to.
async fn given_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, body) = s3(router, "PUT", &format!("/{bucket}"), &[]).await;
    assert_eq!(
        status, 200,
        "setup for case {case:?}: the bucket {bucket:?} must be created, got {status} with \
         {body:?}"
    );
}

/// `PUT /<bucket>/<key>`: one input document, staged through object storage's own write so
/// a run reads only what the object store actually holds.
async fn given_document(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    text: &str,
) {
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{bucket}/{key}"),
        text.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup for case {case:?}: the document {key:?} must be written, got {status} with \
         {body:?}"
    );
}

/// `GET /<bucket>/<key>`: an object as a client reads it back.
async fn object(router: &MultiServiceRouter, bucket: &str, key: &str) -> (u16, String) {
    s3(router, "GET", &format!("/{bucket}/{key}"), &[]).await
}

/// The object a run wrote, parsed as the JSON document it is.
async fn written_json(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) -> Value {
    let (status, body) = object(router, bucket, key).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the run's output object {key:?} must be readable, got {status} with \
         {body:?}"
    );
    serde_json::from_str(&body).unwrap_or_else(|error| {
        panic!("case {case:?}: the output object {key:?} must be JSON, got {body:?}: {error}")
    })
}

/// Every key the bucket holds under `prefix`, as `ListObjectsV2` reports them — the call a
/// client makes to find a finished job's output.
async fn listed_keys(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    prefix: &str,
) -> BTreeSet<String> {
    let (status, body) = s3(
        router,
        "GET",
        &format!("/{bucket}?list-type=2&prefix={}", form_encode(prefix)),
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the bucket {bucket:?} must be listable, got {status} with {body:?}"
    );
    elements(&body, "Key").into_iter().collect()
}

/// Create `role_name` with a trust policy admitting `principal`, through IAM's own
/// `CreateRole`, and answer the ARN IAM reports for it.
///
/// Local for the same reason as the S3 helper above: the role helpers sibling suites export
/// create the roles THEIR rules are about (a session's own role, a training role) with the
/// trust policy those suites need, while every row here turns on WHICH service principal a
/// role admits, so the principal is this helper's one parameter.
async fn create_role(router: &MultiServiceRouter, role_name: &str, principal: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": principal},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, body) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the role {role_name:?} must be created, got {status} with {body:?}"
    );
    // `GetRole`, which is where a client reads the ARN it then passes.
    let (status, document) = iam(router, "GetRole", &[("RoleName", role_name)]).await;
    assert_eq!(
        status, 200,
        "setup: the role {role_name:?} must be readable, got {status} with {document:?}"
    );
    let arn = element(&document, "Arn");
    assert!(
        arn.ends_with(&format!(":role/{role_name}")),
        "setup: GetRole reports the role's ARN, got {document:?}"
    );
    arn
}

/// The data-access role the submissions pass: a role whose trust policy admits this
/// service's principal, holding the object permissions a job's run needs.
async fn given_role(router: &MultiServiceRouter) -> String {
    let arn = create_role(router, ROLE_NAME, SERVICE_PRINCIPAL).await;
    let permissions = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["s3:GetObject", "s3:ListBucket", "s3:PutObject"],
            "Resource": "*",
        }],
    })
    .to_string();
    let (status, body) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "batch-documents"),
            ("PolicyDocument", &permissions),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the role policy must be written, got {status} with {body:?}"
    );
    arn
}

/// A start body carrying everything the model requires — `InputDataConfig`,
/// `OutputDataConfig`, `DataAccessRoleArn` and `LanguageCode` — plus `extra`.
fn start_body(role_arn: &str, input: Value, output: Value, extra: &[(&str, Value)]) -> Value {
    let mut body = json!({
        "InputDataConfig": input,
        "OutputDataConfig": output,
        "DataAccessRoleArn": role_arn,
        "LanguageCode": "en",
    });
    for (member, value) in extra {
        body[*member] = value.clone();
    }
    body
}

/// Submit one job and answer the `JobId` the submission minted.
async fn start_phi_job(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let (status, answer) = comprehend_medical(router, "StartPHIDetectionJob", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the submission must be accepted, got {status} with {answer}"
    );
    answer["JobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the submission answers a JobId, got {answer}"))
        .to_string()
}

/// `DescribePHIDetectionJob`, as a client polling its job calls it.
async fn describe_phi_job(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let (status, answer) = comprehend_medical(
        router,
        "DescribePHIDetectionJob",
        &json!({"JobId": job_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the job {job_id:?} must be describable, got {status} with {answer}"
    );
    answer["ComprehendMedicalAsyncJobProperties"].clone()
}

/// Poll the job until it reports a terminal status, and answer the properties that first
/// reported it.
async fn poll_until_terminal(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let started = Instant::now();
    let mut seen: Vec<String> = Vec::new();
    loop {
        let properties = describe_phi_job(router, case, job_id).await;
        let reported = properties["JobStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if TERMINAL_STATUSES.contains(&reported.as_str()) {
            return properties;
        }
        if seen.last() != Some(&reported) {
            seen.push(reported);
        }
        assert!(
            started.elapsed() < POLL_BUDGET,
            "case {case:?}: the job {job_id:?} never reached a terminal status, saw {seen:?}"
        );
        ::tokio::time::sleep(POLL_STEP).await;
    }
}

/// The entities the SYNCHRONOUS detector reports for `text`, which is the analysis a run
/// writes for a document holding that text.
async fn synchronous_entities(router: &MultiServiceRouter, case: &str, text: &str) -> Value {
    let (status, answer) = comprehend_medical(router, "DetectPHI", &json!({"Text": text})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DetectPHI must answer 200, got {status} with {answer}"
    );
    answer["Entities"].clone()
}

/// The properties `ListPHIDetectionJobs` reports for `job_id`, or `Value::Null` when the
/// listing does not report it at all.
async fn listed_job(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let (status, answer) = comprehend_medical(router, "ListPHIDetectionJobs", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the family must be listable, got {status} with {answer}"
    );
    answer["ComprehendMedicalAsyncJobPropertiesList"]
        .as_array()
        .and_then(|jobs| {
            jobs.iter()
                .find(|job| job["JobId"].as_str() == Some(job_id))
                .cloned()
        })
        .unwrap_or(Value::Null)
}

/// The name a document's output file is written under: the document's key with the
/// submitted input prefix removed, and — for a prefix that names the document itself,
/// leaving nothing under it — the object's own last segment.
///
/// Derived here from the rule rather than called from the service's own code on purpose: a
/// check that asked the implementation where it put its output would assert nothing about
/// where the output belongs.
fn output_name<'a>(input_prefix: &str, key: &'a str) -> &'a str {
    let relative = key
        .strip_prefix(input_prefix)
        .unwrap_or(key)
        .trim_start_matches('/');
    if relative.is_empty() {
        key.rsplit('/').next().unwrap_or(key)
    } else {
        relative
    }
}

/// The directory the run of `job_id` wrote into, read off the `ManifestFilePath` the job
/// reports: the path of the file describing the results, with that file's own name removed.
fn output_directory(case: &str, bucket: &str, properties: &Value) -> String {
    let path = properties["ManifestFilePath"].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: a completed run reports its ManifestFilePath, got {properties}")
    });
    let key = path
        .strip_prefix(&format!("s3://{bucket}/"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the manifest must live in {bucket:?}, got {path:?}")
        });
    let directory = key.strip_suffix(MANIFEST_NAME).unwrap_or_else(|| {
        panic!("case {case:?}: the manifest is the file describing the results, got {path:?}")
    });
    directory.to_string()
}

/// One document a row stages, and what the analysis of it must report.
struct Document {
    key: &'static str,
    text: &'static str,
    /// The person name a `NAME` entity must cover, where the text carries one.
    name: Option<&'static str>,
}

/// One row of the run table.
struct Row {
    case: &'static str,
    bucket: &'static str,
    documents: Vec<Document>,
    /// `InputDataConfig.S3Key`, as the submission spells it.
    input_key: &'static str,
    /// `OutputDataConfig.S3Key`, or `None` for a submission that omits the member.
    output_key: Option<&'static str>,
    /// Whether the run can be carried out at all: an input the object store holds no
    /// document under analyses nothing.
    runs: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one document under an input prefix",
            bucket: "phi-batch-one",
            documents: vec![Document {
                key: "input/doc.txt",
                text: "Patient Jane Doe, age 52, was seen on 03/09/2023 for a routine \
                       follow-up visit.",
                name: Some("Jane Doe"),
            }],
            input_key: "input/",
            output_key: Some("output/"),
            runs: true,
        },
        Row {
            case: "several documents, one of them under a nested key, into a nested output \
                   prefix",
            bucket: "phi-batch-many",
            documents: vec![
                Document {
                    key: "corpus/first.txt",
                    text: "Mr. Okonkwo reported no pain at the follow-up.",
                    name: Some("Okonkwo"),
                },
                Document {
                    key: "corpus/deeper/second.txt",
                    text: "Results were sent to jane.roe@example.com on 04/01/2024.",
                    name: None,
                },
                Document {
                    key: "corpus/third.txt",
                    text: "Blood pressure was stable throughout the visit.",
                    name: None,
                },
            ],
            input_key: "corpus/",
            output_key: Some("results/phi/"),
            runs: true,
        },
        Row {
            case: "an input prefix naming one document rather than a directory, and no \
                   output prefix at all",
            bucket: "phi-batch-single",
            documents: vec![Document {
                key: "only/one.txt",
                text: "Dr. Alice Nguyen signed the note for the patient.",
                name: Some("Alice Nguyen"),
            }],
            input_key: "only/one.txt",
            output_key: None,
            runs: true,
        },
        Row {
            case: "an input prefix the bucket holds no document under",
            bucket: "phi-batch-empty",
            documents: vec![Document {
                key: "elsewhere/doc.txt",
                text: "Patient Jane Doe was seen today.",
                name: Some("Jane Doe"),
            }],
            input_key: "input/",
            output_key: Some("output/"),
            runs: false,
        },
    ]
}

/// A PHI job submitted with a role IAM answers for runs: it reads the documents under the
/// submitted input, writes one output file per document — the entities the synchronous
/// detector reports for that document's text — beside the file describing the results, and
/// only then reports `COMPLETED`. An input that holds no document to analyse reaches the
/// modelled failure state instead and writes nothing.
#[tokio::test]
async fn a_phi_detection_job_writes_the_analysis_of_its_documents_into_object_storage() {
    let router = emulator();
    let role_arn = given_role(&router).await;

    for row in rows() {
        let case = row.case;
        given_bucket(&router, case, row.bucket).await;
        for document in &row.documents {
            given_document(&router, case, row.bucket, document.key, document.text).await;
        }

        let mut output = json!({"S3Bucket": row.bucket});
        if let Some(output_key) = row.output_key {
            output["S3Key"] = json!(output_key);
        }
        let job_id = start_phi_job(
            &router,
            case,
            &start_body(
                &role_arn,
                json!({"S3Bucket": row.bucket, "S3Key": row.input_key}),
                output,
                &[("JobName", json!("phi-batch-job"))],
            ),
        )
        .await;

        let properties = poll_until_terminal(&router, case, &job_id).await;
        let reported = properties["JobStatus"].as_str().unwrap_or_default();

        if !row.runs {
            assert_eq!(
                reported, "FAILED",
                "case {case:?}: a run that could not be carried out reaches the modelled \
                 failure state, got {properties}"
            );
            let reason = properties["Message"].as_str().unwrap_or_default();
            assert!(
                reason.contains(&format!("s3://{}/{}", row.bucket, row.input_key)),
                "case {case:?}: the failure names the input it is about, got {reason:?}"
            );
            assert!(
                properties["ManifestFilePath"].is_null(),
                "case {case:?}: a run that produced no results describes none, got {properties}"
            );
            // Nothing was written: under a submitted output prefix the bucket holds
            // nothing at all, and where no prefix was submitted the bucket still holds
            // only the documents this row staged.
            let written = listed_keys(
                &router,
                case,
                row.bucket,
                row.output_key.unwrap_or_default(),
            )
            .await;
            let staged: BTreeSet<String> = match row.output_key {
                Some(_) => BTreeSet::new(),
                None => row
                    .documents
                    .iter()
                    .map(|document| document.key.to_string())
                    .collect(),
            };
            assert_eq!(
                written, staged,
                "case {case:?}: a run that analysed nothing writes nothing, expected \
                 {staged:?}, got {written:?}"
            );
            continue;
        }

        assert_eq!(
            reported, "COMPLETED",
            "case {case:?}: a run that analysed its documents and wrote its output reports \
             COMPLETED, got {properties}"
        );
        // Both members are read out explicitly: an absent SubmitTime must fail this, not
        // satisfy it by comparing as the smaller of two `Option`s.
        let submitted = properties["SubmitTime"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: a job reports the time it was submitted, got {properties}")
        });
        let ended = properties["EndTime"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: a finished job reports the time it ended, got {properties}")
        });
        assert!(
            ended > submitted,
            "case {case:?}: a finished job carries an EndTime after its SubmitTime, got \
             {ended} and {submitted}"
        );

        // Where the output went: the directory the submitted prefix names, with the job's
        // own id in it so one job's output cannot overwrite another's.
        let directory = output_directory(case, row.bucket, &properties);
        let submitted_prefix = match row.output_key.unwrap_or("").trim_end_matches('/') {
            "" => String::new(),
            trimmed => format!("{trimmed}/"),
        };
        assert!(
            directory.starts_with(&submitted_prefix) && directory.contains(&job_id),
            "case {case:?}: the output directory must sit under {submitted_prefix:?} and name \
             the job, got {directory:?}"
        );

        // Exactly one output file per input document, named for that document, beside the
        // one file describing the results.
        let expected: BTreeSet<String> = row
            .documents
            .iter()
            .map(|document| {
                format!(
                    "{directory}{}.out",
                    output_name(row.input_key, document.key)
                )
            })
            .chain([format!("{directory}{MANIFEST_NAME}")])
            .collect();
        let written = listed_keys(&router, case, row.bucket, &directory).await;
        assert_eq!(
            written, expected,
            "case {case:?}: the run must write one output file per document and one manifest, \
             expected {expected:?}, got {written:?}"
        );

        let manifest = written_json(
            &router,
            case,
            row.bucket,
            &format!("{directory}{MANIFEST_NAME}"),
        )
        .await;
        assert_eq!(
            manifest["DocumentCount"].as_u64(),
            Some(row.documents.len() as u64),
            "case {case:?}: the manifest describes every analysed document, got {manifest}"
        );

        for document in &row.documents {
            let key = format!(
                "{directory}{}.out",
                output_name(row.input_key, document.key)
            );
            let analysed = written_json(&router, case, row.bucket, &key).await;
            // The output of a document IS the analysis of that document's own text: the
            // same entities the synchronous detector answers with for the same text.
            let expected_entities = synchronous_entities(&router, case, document.text).await;
            assert_eq!(
                analysed["Entities"], expected_entities,
                "case {case:?}: the output {key:?} must carry the entities of its own text, \
                 expected {expected_entities}, got {analysed}"
            );

            let entities = analysed["Entities"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for entity in &entities {
                assert_eq!(
                    entity["Category"].as_str(),
                    Some(PHI_CATEGORY),
                    "case {case:?}: a PHI detection reports protected-health-information \
                     entities, got {entity}"
                );
                // The offsets select the entity's own text out of the document, so the
                // output points back into the document it was produced from.
                let begin = entity["BeginOffset"].as_u64().unwrap_or_default() as usize;
                let end = entity["EndOffset"].as_u64().unwrap_or_default() as usize;
                let covered: String = document
                    .text
                    .chars()
                    .skip(begin)
                    .take(end.saturating_sub(begin))
                    .collect();
                assert_eq!(
                    entity["Text"].as_str(),
                    Some(covered.as_str()),
                    "case {case:?}: the offsets of {entity} must select its own Text out of \
                     the document, got {covered:?}"
                );
            }

            if let Some(name) = document.name {
                assert!(
                    document.text.contains(name),
                    "case {case:?}: the row's person name {name:?} must be IN the document it is \
                     expected from, got {:?}",
                    document.text
                );
                // The whole name, not a part of it: an entity that covered one letter of it
                // would not have placed the person in the document.
                let covers_the_name = entities.iter().any(|entity| {
                    entity["Type"].as_str() == Some(NAME_TYPE)
                        && entity["Text"].as_str() == Some(name)
                });
                assert!(
                    covers_the_name,
                    "case {case:?}: the output {key:?} must cover the person name {name:?} with \
                     a {NAME_TYPE} entity, got {analysed}"
                );
            }
        }

        // The finished job is projected identically by the family's listing, which is where
        // a client that kept no JobId finds the results.
        let listed = listed_job(&router, case, &job_id).await;
        for member in ["JobStatus", "ManifestFilePath", "EndTime", "JobName"] {
            assert_eq!(
                listed[member], properties[member],
                "case {case:?}: the listing must report the job's {member} as describe does, \
                 expected {} got {}",
                properties[member], listed[member]
            );
        }
    }
}

/// One row of the token table: what the repeat changes, and whether that makes it a repeat
/// of the first submission or a submission of its own.
struct TokenCase {
    case: &'static str,
    extra: Vec<(&'static str, Value)>,
    input_key: &'static str,
    same_submission: bool,
}

/// `ClientRequestToken` identifies ONE submission: repeating that submission names the job
/// it already created and runs it once, while the same token carrying different parameters
/// starts an independent job with its own output and leaves the first job exactly as it was.
#[tokio::test]
async fn one_client_request_token_identifies_one_batch_submission() {
    let router = emulator();
    let role_arn = given_role(&router).await;
    let bucket = "phi-batch-token";
    let setup = "the first submission";
    given_bucket(&router, setup, bucket).await;
    given_document(
        &router,
        setup,
        bucket,
        "input/doc.txt",
        "Patient Jane Doe, age 52, was seen on 03/09/2023.",
    )
    .await;
    given_document(
        &router,
        setup,
        bucket,
        "other/doc.txt",
        "Mr. Okonkwo reported no pain.",
    )
    .await;

    let token = json!("sim2real-idempotency-token");
    let first_extra = vec![
        ("ClientRequestToken", token.clone()),
        ("JobName", json!("token-job")),
    ];
    let first = start_phi_job(
        &router,
        setup,
        &start_body(
            &role_arn,
            json!({"S3Bucket": bucket, "S3Key": "input/"}),
            json!({"S3Bucket": bucket, "S3Key": "output/"}),
            &first_extra,
        ),
    )
    .await;
    let first_properties = poll_until_terminal(&router, setup, &first).await;
    assert_eq!(
        first_properties["JobStatus"].as_str(),
        Some("COMPLETED"),
        "case {setup:?}: the first job must complete before the repeats, got {first_properties}"
    );
    let after_first = listed_keys(&router, setup, bucket, "output/").await;
    assert!(
        !after_first.is_empty(),
        "case {setup:?}: the first run must have written its output, got {after_first:?}"
    );

    let rows = vec![
        TokenCase {
            case: "the identical submission again",
            extra: first_extra.clone(),
            input_key: "input/",
            same_submission: true,
        },
        TokenCase {
            case: "the same token with a changed JobName",
            extra: vec![
                ("ClientRequestToken", token.clone()),
                ("JobName", json!("token-job-CHANGED")),
            ],
            input_key: "input/",
            same_submission: false,
        },
        TokenCase {
            case: "the same token with no JobName at all",
            extra: vec![("ClientRequestToken", token.clone())],
            input_key: "input/",
            same_submission: false,
        },
        TokenCase {
            case: "the same token with a changed input prefix",
            extra: first_extra.clone(),
            input_key: "other/",
            same_submission: false,
        },
    ];

    let mut minted: BTreeSet<String> = BTreeSet::new();
    minted.insert(first.clone());
    for row in &rows {
        let case = row.case;
        let repeated = start_phi_job(
            &router,
            case,
            &start_body(
                &role_arn,
                json!({"S3Bucket": bucket, "S3Key": row.input_key}),
                json!({"S3Bucket": bucket, "S3Key": "output/"}),
                &row.extra,
            ),
        )
        .await;

        if row.same_submission {
            assert_eq!(
                repeated, first,
                "case {case:?}: repeating one submission must name the job it created, got \
                 {repeated:?}"
            );
            // The repeat is not a second run: the output the first run wrote is all the
            // output there is, and the first job's record is untouched.
            let after_repeat = listed_keys(&router, case, bucket, "output/").await;
            assert_eq!(
                after_repeat, after_first,
                "case {case:?}: repeating one submission must not run it again, expected \
                 {after_first:?}, got {after_repeat:?}"
            );
            continue;
        }

        assert!(
            minted.insert(repeated.clone()),
            "case {case:?}: a token carrying different parameters is another submission and \
             must start a job of its own, got the already-minted {repeated:?}"
        );
        let properties = poll_until_terminal(&router, case, &repeated).await;
        assert_eq!(
            properties["JobStatus"].as_str(),
            Some("COMPLETED"),
            "case {case:?}: the second submission's own run must be carried out, got \
             {properties}"
        );
        assert_eq!(
            properties["JobName"], row.extra.iter().find(|(member, _)| *member == "JobName")
                .map(|(_, value)| value.clone()).unwrap_or(Value::Null),
            "case {case:?}: the new job carries the request it was submitted with, got \
             {properties}"
        );
        // Its output is its own, written beside the first job's rather than over it.
        let directory = output_directory(case, bucket, &properties);
        let first_directory = output_directory(case, bucket, &first_properties);
        assert_ne!(
            directory, first_directory,
            "case {case:?}: each job writes into its own output directory, got {directory:?}"
        );
        let written = listed_keys(&router, case, bucket, &directory).await;
        assert!(
            written.len() >= 2,
            "case {case:?}: the second job's run writes its own output and manifest, got \
             {written:?}"
        );

        // The first job is left exactly as it was, output included.
        let first_again = describe_phi_job(&router, case, &first).await;
        for member in [
            "JobName",
            "JobStatus",
            "SubmitTime",
            "EndTime",
            "ManifestFilePath",
            "InputDataConfig",
            "OutputDataConfig",
        ] {
            assert_eq!(
                first_again[member], first_properties[member],
                "case {case:?}: the first job's {member} must be untouched, expected {} got {}",
                first_properties[member], first_again[member]
            );
        }
        let first_output = listed_keys(&router, case, bucket, &first_directory).await;
        assert_eq!(
            first_output,
            after_first
                .iter()
                .filter(|key| key.starts_with(&first_directory))
                .cloned()
                .collect::<BTreeSet<String>>(),
            "case {case:?}: the first job's output must be untouched, got {first_output:?}"
        );
    }
}

/// A `JobId` the family holds no job for is refused with the modelled not-found shape and
/// one fixed sentence that echoes nothing of the request, while a value outside the
/// member's modelled constraints keeps its constraint report and an id that does name a job
/// is answered.
#[tokio::test]
async fn an_id_naming_no_batch_job_is_refused_with_one_fixed_sentence() {
    let router = emulator();
    let role_arn = given_role(&router).await;

    // A job that DOES exist, so the refusals below cannot be explained by a store that
    // answers nothing. Its run has no documents to read, which is beside the point here.
    let existing = start_phi_job(
        &router,
        "a job of this family",
        &start_body(
            &role_arn,
            json!({"S3Bucket": "phi-batch-absent", "S3Key": "input/"}),
            json!({"S3Bucket": "phi-batch-absent", "S3Key": "output/"}),
            &[],
        ),
    )
    .await;

    // Every value is inside `JobId`'s modelled length 1..32 and pattern
    // `^([\p{L}\p{Z}\p{N}_.:/=+\-%@]*)$`
    // (`smithy_query(query="operation",operation="DescribePHIDetectionJob")`), so each
    // request reaches the handler.
    let absent: Vec<(&str, String)> = vec![
        ("an id nothing ever minted", "doesnotexist".to_string()),
        ("an id at the modelled length maximum", "a".repeat(32)),
        ("an id at the modelled length minimum", "a".to_string()),
        (
            "an id of the characters the pattern admits",
            "a.b_c:d/e=f+g%h@i 1".to_string(),
        ),
    ];

    for operation in ["DescribePHIDetectionJob", "StopPHIDetectionJob"] {
        for (case, job_id) in &absent {
            let (status, answer) =
                comprehend_medical(&router, operation, &json!({"JobId": job_id})).await;
            assert_eq!(
                status, JOB_NOT_FOUND_STATUS,
                "case {operation:?} / {case:?}: expected the status the recorded absent-job \
                 refusals carry ({JOB_NOT_FOUND_STATUS}), got {status} with {answer}"
            );
            assert_eq!(
                answer["__type"].as_str(),
                Some("ResourceNotFoundException"),
                "case {operation:?} / {case:?}: expected the modelled not-found shape, got \
                 {answer}"
            );
            assert_eq!(
                answer["Message"].as_str(),
                Some(JOB_NOT_FOUND),
                "case {operation:?} / {case:?}: expected {JOB_NOT_FOUND:?}, got {answer}"
            );
            assert!(
                !answer["Message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(job_id.as_str()),
                "case {operation:?} / {case:?}: the refusal must not echo the id, got {answer}"
            );
        }

        // A value outside the member's constraints is a different negative class, told
        // apart by its constraint report rather than by the sentence above.
        let (status, answer) = comprehend_medical(
            &router,
            operation,
            &json!({"JobId": "not#a#valid#job#id!"}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {operation:?}: a malformed id is refused with 400, got {status} with {answer}"
        );
        let message = answer["message"]
            .as_str()
            .or_else(|| answer["Message"].as_str())
            .unwrap_or_default();
        assert!(
            message.contains("failed to satisfy constraint"),
            "case {operation:?}: a malformed id keeps its constraint report, got {answer}"
        );
        assert_ne!(
            message, JOB_NOT_FOUND,
            "case {operation:?}: a malformed id is not an absent job, got {answer}"
        );
    }

    let properties = describe_phi_job(&router, "a job of this family", &existing).await;
    assert_eq!(
        properties["JobId"].as_str(),
        Some(existing.as_str()),
        "case {:?}: an id that names a job is answered with that job, got {properties}",
        "a job of this family"
    );
}

/// A job RUNS AS the role its submission names, so a run only happens at all once this
/// account's own IAM has answered that this service may assume that role: a role IAM holds
/// nothing under, and one whose trust policy admits another service, back no job — the
/// submission is refused with the modelled client error, nothing is minted and nothing is
/// recorded — while the role IAM does answer for is accepted and its job's run is carried
/// out.
///
/// This is what makes the three services on one engine load-bearing here: the refusals
/// below are grounded in what IAM ANSWERED, so nothing but a real IAM beside the service
/// can produce them.
#[tokio::test]
async fn a_batch_submission_runs_only_under_a_role_iam_answers_for() {
    let router = emulator();
    let bucket = "phi-batch-role";
    let setup = "the role a job runs as";
    given_bucket(&router, setup, bucket).await;
    given_document(
        &router,
        setup,
        bucket,
        "input/doc.txt",
        "Patient Jane Doe, age 52, was seen on 03/09/2023.",
    )
    .await;

    let trusted = given_role(&router).await;
    let another_service = create_role(&router, "comprehend-medical-other-role", OTHER_PRINCIPAL).await;
    // An ARN of THIS account's own namespace — so IAM is asked about it — naming a role no
    // create ever made.
    let (namespace, _) = trusted
        .rsplit_once('/')
        .expect("the role ARN IAM reported names a role");
    let absent = format!("{namespace}/comprehend-medical-absent-role");

    // (case, the DataAccessRoleArn the submission names, whether IAM lets the job run)
    let rows: Vec<(&str, String, bool)> = vec![
        (
            "a role whose trust policy admits this service",
            trusted.clone(),
            true,
        ),
        (
            "a role whose trust policy admits another service",
            another_service,
            false,
        ),
        (
            "a role this account's IAM holds nothing under",
            absent,
            false,
        ),
    ];

    for (case, role_arn, runs) in &rows {
        let body = start_body(
            role_arn,
            json!({"S3Bucket": bucket, "S3Key": "input/"}),
            json!({"S3Bucket": bucket, "S3Key": "output/"}),
            &[("JobName", json!("role-gated-job"))],
        );
        let (status, answer) = comprehend_medical(&router, "StartPHIDetectionJob", &body).await;

        if !runs {
            assert_eq!(
                status, 400,
                "case {case:?}: a role this service cannot assume backs no job, got {status} \
                 with {answer}"
            );
            assert_eq!(
                answer["__type"].as_str(),
                Some("InvalidRequestException"),
                "case {case:?}: expected the modelled client error, got {answer}"
            );
            assert_eq!(
                answer["Message"].as_str(),
                Some(ROLE_NOT_TRUSTED),
                "case {case:?}: expected {ROLE_NOT_TRUSTED:?}, got {answer}"
            );
            assert!(
                answer["JobId"].is_null(),
                "case {case:?}: a refused submission mints no JobId, got {answer}"
            );
            // Nothing was recorded either: no job of this family carries that role.
            let (status, listing) =
                comprehend_medical(&router, "ListPHIDetectionJobs", &json!({})).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the family must be listable, got {status} with {listing}"
            );
            let recorded = listing["ComprehendMedicalAsyncJobPropertiesList"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                recorded.iter().all(|job| job["DataAccessRoleArn"].as_str()
                    != Some(role_arn.as_str())),
                "case {case:?}: a refused submission records no job, got {listing}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: a role IAM answers for backs a job, got {status} with {answer}"
        );
        let job_id = answer["JobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a JobId, got {answer}"))
            .to_string();
        let properties = poll_until_terminal(&router, case, &job_id).await;
        assert_eq!(
            properties["JobStatus"].as_str(),
            Some("COMPLETED"),
            "case {case:?}: the job must run under the role it was submitted with, got \
             {properties}"
        );
        assert_eq!(
            properties["DataAccessRoleArn"].as_str(),
            Some(role_arn.as_str()),
            "case {case:?}: the job reports the role it runs as, got {properties}"
        );
        let written = listed_keys(&router, case, bucket, "output/").await;
        assert!(
            !written.is_empty(),
            "case {case:?}: the run of an accepted submission writes its output, got \
             {written:?}"
        );
    }
}
