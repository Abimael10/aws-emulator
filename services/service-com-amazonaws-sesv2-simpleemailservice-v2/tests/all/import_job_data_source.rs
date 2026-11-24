// SPDX-License-Identifier: CC0-1.0

//! The Amazon S3 data source an import job's run reads, and which terminal `JobStatus` the
//! run therefore reaches — end to end over HTTP, with this service and the account's own
//! Amazon S3 mounted on ONE engine, so the object a run resolves is an object a client
//! really staged through `PutObject`.
//!
//! The rule these cases pin: `JobStatus` documents `COMPLETED` as "job has completed
//! processing successfully", so a run reaches it only where it had the source it was created
//! to process — the object `ImportDataSource$S3Url` names ("an Amazon S3 URL in the format
//! s3://<bucket_name>/<object>"). A run whose source the account's object storage answers it
//! holds no object at reaches `FAILED` instead, carrying the `FailureInfo` that says why,
//! and `GetImportJob` and `ListImportJobs` report that same status because the answer is
//! recorded on the job rather than re-decided per read.
//!
//! The table spans what the rule generalises over: whether the bucket exists, whether the
//! object under it exists, where the object sits (the bucket root, nested prefixes), the
//! object's own content (bytes, and none at all), a URL that names no object for any store
//! to hold, a prefix-shaped key, both `ImportDestinationType` destinations and both
//! `DataFormat` values.
//!
//! Nothing here waits for a span of time: the instants the run's transitions fall due at are
//! moved on the stored record, exactly as the import-job lifecycle suite beside this one
//! does, so a case asserts the status the clock has made due without sleeping for it.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::import_source::source_absent_message;
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::{
    ComAmazonawsSesv2SimpleemailserviceV2State, IMPORT_JOB_COMPLETED, IMPORT_JOB_CREATED,
    IMPORT_JOB_FAILED, IMPORT_JOB_PROCESSING,
};
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region the cases execute in: the one their `Authorization` credential scope names,
/// and the one the hop into the object store is addressed with.
const REGION: &str = "us-east-1";

/// Long enough that a transition held this far out cannot fall due while a case runs.
const HELD: Duration = Duration::from_secs(3600);

/// Just enough to put a due instant in the past.
const ELAPSED: Duration = Duration::from_secs(1);

/// The bucket the staged sources live in, where the bucket is not what the row varies.
const BUCKET: &str = "ses-import-sources";

/// One row of the table: what the account's object storage holds, the URL the create
/// supplies, and the terminal status the run then reaches.
struct Row {
    case: &'static str,
    /// The bucket to create first, if any.
    bucket: Option<&'static str>,
    /// The object to stage, if any: its key and its bytes.
    object: Option<(&'static str, &'static str)>,
    /// `ImportDataSource$S3Url`, as the create supplies it.
    s3_url: &'static str,
    /// `ImportDataSource$DataFormat`.
    data_format: &'static str,
    /// The contact list the destination names, or `None` for a suppression-list
    /// destination.
    contact_list: Option<&'static str>,
    /// The terminal `JobStatus` the run reaches.
    status: &'static str,
}

/// The rows span what the account's object storage holds for the URL the create supplied.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a bucket the account's object storage does not hold",
            bucket: None,
            object: None,
            s3_url: "s3://ses-imports-never-created/suppression.csv",
            data_format: "CSV",
            contact_list: None,
            status: IMPORT_JOB_FAILED,
        },
        Row {
            case: "a bucket that is there with nothing at the key",
            bucket: Some(BUCKET),
            object: None,
            s3_url: "s3://ses-import-sources/suppression.csv",
            data_format: "CSV",
            contact_list: None,
            status: IMPORT_JOB_FAILED,
        },
        Row {
            case: "an object staged at the bucket root",
            bucket: Some(BUCKET),
            object: Some(("suppression.csv", "reader@example.net,BOUNCE\n")),
            s3_url: "s3://ses-import-sources/suppression.csv",
            data_format: "CSV",
            contact_list: None,
            status: IMPORT_JOB_COMPLETED,
        },
        Row {
            case: "an object staged under nested prefixes, for a contact list",
            bucket: Some(BUCKET),
            object: Some((
                "2026/10/29/contacts.json",
                "{\"emailAddress\":\"reader@example.net\"}\n",
            )),
            s3_url: "s3://ses-import-sources/2026/10/29/contacts.json",
            data_format: "JSON",
            contact_list: Some("newsletter"),
            status: IMPORT_JOB_COMPLETED,
        },
        Row {
            case: "an object that is there and empty, which a read of the source still finds",
            bucket: Some(BUCKET),
            object: Some(("empty.csv", "")),
            s3_url: "s3://ses-import-sources/empty.csv",
            data_format: "CSV",
            contact_list: None,
            status: IMPORT_JOB_COMPLETED,
        },
        Row {
            case: "a prefix-shaped key with nothing at it",
            bucket: Some(BUCKET),
            object: Some(("contacts/one.csv", "reader@example.net\n")),
            s3_url: "s3://ses-import-sources/contacts/",
            data_format: "CSV",
            contact_list: Some("newsletter"),
            status: IMPORT_JOB_FAILED,
        },
        Row {
            case: "an authority Amazon S3 holds no bucket under, which names no object at all",
            bucket: Some(BUCKET),
            object: Some(("suppression.csv", "reader@example.net,BOUNCE\n")),
            s3_url: "s3://SES-Import-Sources/suppression.csv",
            data_format: "CSV",
            contact_list: None,
            status: IMPORT_JOB_FAILED,
        },
    ]
}

/// One answered request: the status and the parsed body.
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `""` when absent.
    fn text(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// This service and the account's own Amazon S3 on one engine, behind one router.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These rows exercise handler behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { engine, router }
    }

    /// One call of this service's REST API.
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let host = format!("email.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR0000000/20260101/{REGION}/ses/aws4_request, SignedHeaders=host, Signature=00"
                ),
            )
            .body(payload)
            .expect("build request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply { status, body }
    }

    /// An Amazon S3 client over this emulator's own in-process callback — the same hop the
    /// handler resolves a data source over.
    fn s3(&self) -> aws_sdk_s3::Client {
        let callback = smithy_server_runtime::EmbeddedEmulator::from_router(self.router.clone());
        service_common::s3_client::s3_client(&callback, REGION)
    }

    /// Stage what `row` says the account's object storage holds.
    async fn given_storage(&self, row: &Row) {
        let Some(bucket) = row.bucket else {
            return;
        };
        self.s3()
            .create_bucket()
            .bucket(bucket)
            .send()
            .await
            .unwrap_or_else(|error| panic!("case {:?}: creating {bucket}: {error}", row.case));
        let Some((key, bytes)) = row.object else {
            return;
        };
        self.s3()
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(aws_sdk_s3::primitives::ByteStream::from(
                bytes.as_bytes().to_vec(),
            ))
            .send()
            .await
            .unwrap_or_else(|error| panic!("case {:?}: staging {bucket}/{key}: {error}", row.case));
    }

    /// Create the contact list `row` names, when it names one.
    async fn given_contact_list(&self, row: &Row) {
        let Some(name) = row.contact_list else {
            return;
        };
        let reply = self
            .call(
                "POST",
                "/v2/email/contact-lists",
                Some(json!({ "ContactListName": name })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: the destination list must be created, got {} {:?}",
            row.case, reply.status, reply.body
        );
    }

    /// Create the import job `row` describes, and answer the id minted for it.
    async fn create_job(&self, row: &Row) -> String {
        let destination = match row.contact_list {
            Some(name) => json!({
                "ContactListDestination": {
                    "ContactListName": name,
                    "ContactListImportAction": "PUT",
                },
            }),
            None => json!({
                "SuppressionListDestination": { "SuppressionListImportAction": "PUT" },
            }),
        };
        let reply = self
            .call(
                "POST",
                "/v2/email/import-jobs",
                Some(json!({
                    "ImportDestination": destination,
                    "ImportDataSource": {
                        "S3Url": row.s3_url,
                        "DataFormat": row.data_format,
                    },
                })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: a create of a modelled destination is served, got {} {:?}",
            row.case, reply.status, reply.body
        );
        let job_id = reply.text("JobId");
        assert!(
            !job_id.is_empty(),
            "case {:?}: expected the minted JobId, got {:?}",
            row.case,
            reply.body
        );
        job_id
    }

    /// `GetImportJob`.
    async fn get_job(&self, job_id: &str) -> Reply {
        self.call("GET", &format!("/v2/email/import-jobs/{job_id}"), None)
            .await
    }

    /// `ListImportJobs`, which the model binds to a POST of its own path.
    async fn list_jobs(&self) -> Reply {
        self.call("POST", "/v2/email/import-jobs/list", Some(json!({})))
            .await
    }

    /// Hold both of `job_id`'s transitions off, so an assertion about the status it was
    /// created in cannot race the span.
    fn hold_created(&self, job_id: &str) {
        let now = SystemTime::now();
        self.set_due(job_id, now + HELD, now + HELD + HELD);
    }

    /// Bring the start of `job_id`'s run due, without waiting for it, and hold its terminal
    /// transition off.
    fn bring_processing_due(&self, job_id: &str) {
        let now = SystemTime::now();
        self.set_due(job_id, now - ELAPSED, now + HELD);
    }

    /// Bring the terminal transition of `job_id`'s run due, without waiting for it.
    fn bring_completion_due(&self, job_id: &str) {
        let now = SystemTime::now();
        self.set_due(job_id, now - ELAPSED - ELAPSED, now - ELAPSED);
    }

    /// Move the instants `job_id`'s transitions fall due at.
    fn set_due(&self, job_id: &str, processing_due_at: SystemTime, completion_due_at: SystemTime) {
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let mut store = state.lock();
        let record = store
            .import_jobs
            .get_mut(job_id)
            .unwrap_or_else(|| panic!("the job {job_id:?} must be in the store"));
        record.processing_due_at = processing_due_at;
        record.completion_due_at = completion_due_at;
    }
}

/// The `FailureInfo$ErrorMessage` a reply carries, or `""` when it carries no failure
/// details at all.
fn failure_message(reply: &Reply) -> String {
    reply
        .body
        .pointer("/FailureInfo/ErrorMessage")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The status `ListImportJobs` reports for `job_id`, or `""` when it lists no such job.
fn listed_status(reply: &Reply, job_id: &str) -> String {
    reply
        .body
        .get("ImportJobs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|summary| summary.get("JobId").and_then(Value::as_str) == Some(job_id))
        .and_then(|summary| summary.get("JobStatus"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn the_terminal_status_is_decided_by_the_data_source_the_run_reads() {
    for row in rows() {
        let case = row.case;
        let ses = Emulator::new();
        ses.given_storage(&row).await;
        ses.given_contact_list(&row).await;
        let job_id = ses.create_job(&row).await;

        // Before the run reaches its terminal transition nothing has read the source, so
        // no job carries failure details however unreadable that source is.
        for (phase, expected) in [
            ("held", IMPORT_JOB_CREATED),
            ("processing", IMPORT_JOB_PROCESSING),
        ] {
            match phase {
                "held" => ses.hold_created(&job_id),
                _ => ses.bring_processing_due(&job_id),
            }
            let reply = ses.get_job(&job_id).await;
            assert_eq!(
                (reply.status, reply.text("JobStatus").as_str()),
                (200, expected),
                "case {case:?} ({phase}): expected the status the clock has made due, got {} {:?}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.body.get("FailureInfo"),
                None,
                "case {case:?} ({phase}): expected a run that has not read its source to carry no \
                 failure details, got {:?}",
                reply.body.get("FailureInfo")
            );
        }

        // Once the terminal transition is due, the run has read its source and the status
        // is the one that read decided.
        ses.bring_completion_due(&job_id);
        let reply = ses.get_job(&job_id).await;
        assert_eq!(
            (reply.status, reply.text("JobStatus").as_str()),
            (200, row.status),
            "case {case:?}: expected the terminal status the data source decides, got {} {:?}",
            reply.status,
            reply.body
        );
        let expected_message = match row.status {
            IMPORT_JOB_FAILED => source_absent_message(row.s3_url),
            _ => String::new(),
        };
        assert_eq!(
            failure_message(&reply),
            expected_message,
            "case {case:?}: expected the failure details the store's answer words, got {:?}",
            reply.body.get("FailureInfo")
        );
        assert_eq!(
            (
                reply.body.get("ProcessedRecordsCount").and_then(Value::as_i64),
                reply.body.get("FailedRecordsCount").and_then(Value::as_i64),
            ),
            (Some(0), Some(0)),
            "case {case:?}: expected the counts the record holds, got {:?}",
            reply.body
        );

        // A listing reports the same status, and a second read answers the same again: the
        // store's answer is recorded on the job rather than re-decided per read.
        let listed = ses.list_jobs().await;
        assert_eq!(
            listed_status(&listed, &job_id),
            row.status,
            "case {case:?}: expected the listing to report the same status, got {:?}",
            listed.body
        );
        let again = ses.get_job(&job_id).await;
        assert_eq!(
            (again.text("JobStatus"), failure_message(&again)),
            (row.status.to_string(), expected_message),
            "case {case:?}: expected a second read to answer the same, got {:?}",
            again.body
        );
    }
}

#[tokio::test]
async fn a_listing_alone_resolves_the_source_of_a_run_the_clock_has_made_due() {
    // The resolution is owed by the CLOCK, not by one operation: a client that only ever
    // lists its import jobs is told the same terminal status a read would have told it.
    let ses = Emulator::new();
    let row = Row {
        case: "listed without ever being read",
        bucket: None,
        object: None,
        s3_url: "s3://ses-imports-never-created/suppression.csv",
        data_format: "CSV",
        contact_list: None,
        status: IMPORT_JOB_FAILED,
    };
    let job_id = ses.create_job(&row).await;
    ses.bring_completion_due(&job_id);

    let listed = ses.list_jobs().await;
    assert_eq!(
        listed_status(&listed, &job_id),
        IMPORT_JOB_FAILED,
        "expected the listing to report the run that could not read its source as failed, got \
         {:?}",
        listed.body
    );
    let read = ses.get_job(&job_id).await;
    assert_eq!(
        (read.text("JobStatus"), failure_message(&read)),
        (
            IMPORT_JOB_FAILED.to_string(),
            source_absent_message(row.s3_url)
        ),
        "expected the read to report what the listing already established, got {:?}",
        read.body
    );
}

#[tokio::test]
async fn a_run_whose_source_nothing_can_be_asked_about_is_not_failed_on_an_assumption() {
    // With no object store mounted beside this service, nothing answers about the source:
    // nothing is established, so the run is not reported as having failed to read it.
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    let ses = Emulator { engine, router };

    let row = Row {
        case: "no object store to ask",
        bucket: None,
        object: None,
        s3_url: "s3://ses-import-sources/suppression.csv",
        data_format: "CSV",
        contact_list: None,
        status: IMPORT_JOB_COMPLETED,
    };
    let job_id = ses.create_job(&row).await;
    ses.bring_completion_due(&job_id);

    let reply = ses.get_job(&job_id).await;
    assert_eq!(
        (reply.text("JobStatus"), failure_message(&reply)),
        (IMPORT_JOB_COMPLETED.to_string(), String::new()),
        "expected a run nothing could answer about to carry no diagnosed failure, got {:?}",
        reply.body
    );
}
