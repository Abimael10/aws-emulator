// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the JOBS plane's read-back rule, over the two resources
//! this plane lets a caller create and then read: `DescribeJob` for a job and
//! `DescribeJobTemplate` for a job template.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 `PUT /jobs/{jobId}`, `GET /jobs/{jobId}` and `DELETE /jobs/{jobId}`, and
//! `PUT /job-templates/{jobTemplateId}` and `GET /job-templates/{jobTemplateId}`, signed
//! for the `iot` signing name — so what is asserted is what a client reads back through
//! the modelled surface, not the state struct.
//!
//! The rule under test: an identifier a create hands back addresses a record in the
//! plane that owns it, so `DescribeJob` answers 200 for the `jobId` a job was created
//! under, reporting the identity, the targets, the selection, the timeout, the
//! parameters and the document source that create persisted, plus the progress its
//! executions have made — and a `jobId` no record holds is the modelled
//! `ResourceNotFoundException` (HTTP 404), never a success and never a 501.
//!
//! The same rule over a JOB TEMPLATE: `DescribeJobTemplate` answers 200 for the
//! `jobTemplateId` a template was created under, reporting every member of the stored
//! record — the job document it deploys and the rollout, abort, retry, timeout,
//! maintenance-window, destination-package and pre-signed-URL configuration a job
//! created from it is deployed under — while an id the account holds no template under is
//! that same modelled 404. Two further rules come with the create: a template based on a
//! job (`jobArn`) keeps THAT job's document and in-progress timeout, and a request the
//! store cannot take a template for — an id already taken, or no job document named at
//! all — is refused with the class that states why and writes nothing.
//!
//! The tables span what the rules generalise over — for a job, whether it carries a
//! description, a `targetSelection` (and which), an in-progress timeout, substitution
//! parameters, an inline document or a `documentSource`, and how many devices it targets;
//! for a template, the form and length of its id, how it names the document it deploys
//! (inline, by S3 link, or inherited from a job) and which optional configuration members
//! the request carries — rather than one recorded request.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The members BOTH readers of this plane report for EVERY record — the configuration
/// structures a job and a job template are deployed under, a job's per-status counters
/// and whether it is concurrent — which is why it is a child of this suite rather than a
/// second suite beside it. The `#[path]` is what keeps it one: this crate's `tests/all.rs`
/// declares the suites directly under `tests/all/`, and a module of a `#[path]`-declared
/// file resolves next to that file unless it says otherwise.
#[path = "job_description/reported_configuration.rs"]
mod reported_configuration;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// The `JobStatus` a job that is rolling out to its targets carries — one of the
/// model's `JobStatus` values (`IN_PROGRESS`, `CANCELED`, `COMPLETED`,
/// `DELETION_IN_PROGRESS`, `SCHEDULED`).
const STATUS_IN_PROGRESS: &str = "IN_PROGRESS";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name:
/// the scope is how the router picks the claiming service.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    fn text(&self, pointer: &str) -> Option<String> {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn message(&self) -> String {
        self.text("/message").unwrap_or_default()
    }

    /// The `job` member of a `DescribeJob` answer.
    fn job(&self) -> &Value {
        self.body.get("job").unwrap_or(&Value::Null)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
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
            error_code,
            body,
        }
    }

    async fn create_job(&self, job_id: &str, request: &Value) -> Reply {
        self.call("PUT", &format!("/jobs/{job_id}"), Some(request))
            .await
    }

    async fn describe_job(&self, job_id: &str) -> Reply {
        self.call("GET", &format!("/jobs/{job_id}"), None).await
    }

    async fn delete_job(&self, job_id: &str) -> Reply {
        self.call("DELETE", &format!("/jobs/{job_id}?force=true"), None)
            .await
    }

    async fn create_job_template(&self, job_template_id: &str, request: &Value) -> Reply {
        self.call(
            "PUT",
            &format!("/job-templates/{job_template_id}"),
            Some(request),
        )
        .await
    }

    async fn describe_job_template(&self, job_template_id: &str) -> Reply {
        self.call("GET", &format!("/job-templates/{job_template_id}"), None)
            .await
    }
}

/// The thing ARN a target entry names a device by.
fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// The job ARN this plane mints for `job_id` — `Job$jobArn` is "an ARN identifying the
/// job with format arn:aws:iot:region:account:job/jobId".
fn job_arn(job_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:job/{job_id}")
}

/// The job-template ARN this plane mints for `job_template_id`: the `jobtemplate/`
/// resource type AWS IoT names a job template by, under the region the request was signed
/// for and the account this emulator serves.
fn job_template_arn(job_template_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:jobtemplate/{job_template_id}")
}

/// One row of the table: a job to create, and what a read of it must report.
struct JobCase {
    case: &'static str,
    job_id: &'static str,
    /// The devices the job targets, by thing name.
    things: Vec<&'static str>,
    description: Option<&'static str>,
    target_selection: Option<&'static str>,
    document: Option<&'static str>,
    document_source: Option<&'static str>,
    in_progress_timeout_minutes: Option<i64>,
    parameters: Vec<(&'static str, &'static str)>,
}

/// RULE: `DescribeJob` reports the job registered under that `jobId` from the stored
/// record — every member its creator persisted, plus the progress counted from its
/// executions.
#[tokio::test]
async fn a_created_job_reads_back_member_for_member() {
    let emulator = Emulator::new();

    let cases = vec![
        JobCase {
            case: "one device, nothing but a document",
            job_id: "job-minimal",
            things: vec!["device-a"],
            description: None,
            target_selection: None,
            document: Some(r#"{"operation":"reboot"}"#),
            document_source: None,
            in_progress_timeout_minutes: None,
            parameters: Vec::new(),
        },
        JobCase {
            case: "a snapshot job with a description and a timeout",
            job_id: "job-snapshot",
            things: vec!["device-b"],
            description: Some("reboot the fleet"),
            target_selection: Some("SNAPSHOT"),
            document: Some(r#"{"operation":"reboot"}"#),
            document_source: None,
            in_progress_timeout_minutes: Some(5),
            parameters: Vec::new(),
        },
        JobCase {
            case: "a continuous job over three devices",
            job_id: "job-continuous",
            things: vec!["device-c", "device-d", "device-e"],
            description: None,
            target_selection: Some("CONTINUOUS"),
            document: Some(r#"{"operation":"install"}"#),
            document_source: None,
            in_progress_timeout_minutes: Some(10_080),
            parameters: Vec::new(),
        },
        JobCase {
            case: "a job whose document carries substitution parameters",
            job_id: "job-parameters",
            things: vec!["device-f"],
            description: Some("install what the parameters name"),
            target_selection: Some("SNAPSHOT"),
            document: Some(r#"{"operation":"${aws:iot:parameter:action}"}"#),
            document_source: None,
            in_progress_timeout_minutes: None,
            parameters: vec![("action", "install"), ("retries", "2")],
        },
        JobCase {
            case: "a job created from a documentSource",
            job_id: "job-document-source",
            things: vec!["device-g"],
            description: None,
            target_selection: None,
            document: None,
            document_source: Some("https://s3.us-east-1.amazonaws.com/updates/payload.json"),
            in_progress_timeout_minutes: Some(1),
            parameters: Vec::new(),
        },
    ];

    for case in &cases {
        let targets: Vec<String> = case.things.iter().map(|thing| thing_arn(thing)).collect();
        let mut request = json!({ "targets": targets });
        if let Some(description) = case.description {
            request["description"] = json!(description);
        }
        if let Some(target_selection) = case.target_selection {
            request["targetSelection"] = json!(target_selection);
        }
        if let Some(document) = case.document {
            request["document"] = json!(document);
        }
        if let Some(document_source) = case.document_source {
            request["documentSource"] = json!(document_source);
        }
        if let Some(minutes) = case.in_progress_timeout_minutes {
            request["timeoutConfig"] = json!({ "inProgressTimeoutInMinutes": minutes });
        }
        if !case.parameters.is_empty() {
            request["documentParameters"] = Value::Object(
                case.parameters
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), json!(value)))
                    .collect(),
            );
        }

        let created = emulator.create_job(case.job_id, &request).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: the job must be creatable, got {} {:?}",
            case.case, created.status, created.body
        );

        let read = emulator.describe_job(case.job_id).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: expected 200, got {} {:?}",
            case.case, read.status, read.body
        );

        // The identity the create reported is the identity the read answers with.
        assert_eq!(
            (read.text("/job/jobId"), read.text("/job/jobArn")),
            (
                Some(case.job_id.to_string()),
                created.text("/jobArn").or_else(|| Some(job_arn(case.job_id)))
            ),
            "case {:?}: expected the created job's id and ARN, got {:?}",
            case.case,
            read.job()
        );
        assert_eq!(
            read.text("/job/jobArn"),
            Some(job_arn(case.job_id)),
            "case {:?}: expected the modelled job ARN format, got {:?}",
            case.case,
            read.job()
        );
        assert_eq!(
            read.text("/job/status"),
            Some(STATUS_IN_PROGRESS.to_string()),
            "case {:?}: a job that is rolling out is {STATUS_IN_PROGRESS}, got {:?}",
            case.case,
            read.job()
        );
        assert_eq!(
            read.body
                .pointer("/job/targets")
                .and_then(Value::as_array)
                .map(|entries| entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<String>>()),
            Some(targets.clone()),
            "case {:?}: expected the targets the job was created with, got {:?}",
            case.case,
            read.job()
        );
        assert_eq!(
            read.text("/job/description").as_deref(),
            case.description,
            "case {:?}: expected the created description, got {:?}",
            case.case,
            read.job()
        );
        assert_eq!(
            read.text("/job/targetSelection").as_deref(),
            case.target_selection,
            "case {:?}: expected the created targetSelection, got {:?}",
            case.case,
            read.job()
        );
        assert_eq!(
            read.text("/documentSource").as_deref(),
            case.document_source,
            "case {:?}: expected the created documentSource, got {:?}",
            case.case,
            read.body
        );
        assert_eq!(
            read.body
                .pointer("/job/timeoutConfig/inProgressTimeoutInMinutes")
                .and_then(Value::as_i64),
            case.in_progress_timeout_minutes,
            "case {:?}: expected the created timeoutConfig, got {:?}",
            case.case,
            read.job()
        );
        let expected_parameters: BTreeMap<String, String> = case
            .parameters
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let reported_parameters: BTreeMap<String, String> = read
            .body
            .pointer("/job/documentParameters")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            value.as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            reported_parameters, expected_parameters,
            "case {:?}: expected the created documentParameters, got {:?}",
            case.case,
            read.job()
        );
        assert!(
            read.body.pointer("/job/createdAt").is_some(),
            "case {:?}: a job reports when it was created, got {:?}",
            case.case,
            read.job()
        );

        // The progress is COUNTED from the executions the create queued: one QUEUED
        // execution per targeted device, each device listed as a processing target.
        let devices = i64::try_from(case.things.len()).expect("a handful of devices");
        assert_eq!(
            read.body
                .pointer("/job/jobProcessDetails/numberOfQueuedThings")
                .and_then(Value::as_i64),
            Some(devices),
            "case {:?}: expected one queued thing per target, got {:?}",
            case.case,
            read.job()
        );
        let mut processing: Vec<String> = read
            .body
            .pointer("/job/jobProcessDetails/processingTargets")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        processing.sort();
        let mut expected_processing: Vec<String> =
            case.things.iter().map(|thing| (*thing).to_string()).collect();
        expected_processing.sort();
        assert_eq!(
            processing, expected_processing,
            "case {:?}: expected the targeted devices as processing targets, got {:?}",
            case.case,
            read.job()
        );
    }
}

/// RULE: a `jobId` no job was created under is the modelled `ResourceNotFoundException`
/// (HTTP 404) — never a 200 reporting a job that does not exist, and never a 501.
#[tokio::test]
async fn an_unknown_job_id_is_a_404() {
    let emulator = Emulator::new();

    let created = emulator
        .create_job(
            "job-that-exists",
            &json!({
                "targets": [thing_arn("device-a")],
                "document": r#"{"operation":"reboot"}"#,
            }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the job must be creatable, got {} {:?}",
        created.status, created.body
    );

    // An id nobody used, one differing only in case, one differing in a separator, and
    // (below) the id of a job that has been deleted: none of them names a job.
    let unknown = ["job-that-exists-not", "JOB-THAT-EXISTS", "job_other", "x"];
    for job_id in unknown {
        let reply = emulator.describe_job(job_id).await;
        assert_eq!(
            reply.status, 404,
            "case {job_id:?}: expected 404, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {job_id:?}: expected ResourceNotFoundException, got {:?}",
            reply.error_code
        );
        assert_eq!(
            reply.message(),
            format!("Job {job_id} cannot be found."),
            "case {job_id:?}: expected the refusal to name the job, got {:?}",
            reply.body
        );
    }

    // Round trip the other way: a deleted job is a job no read finds.
    let deleted = emulator.delete_job("job-that-exists").await;
    assert_eq!(
        deleted.status, 200,
        "case \"a forced delete\": expected 200, got {} {:?}",
        deleted.status, deleted.body
    );
    let gone = emulator.describe_job("job-that-exists").await;
    assert_eq!(
        gone.status, 404,
        "case \"a deleted job\": expected 404, got {} {:?}",
        gone.status, gone.body
    );
    assert_eq!(
        gone.error_code.as_deref(),
        Some("ResourceNotFoundException"),
        "case \"a deleted job\": expected ResourceNotFoundException, got {:?}",
        gone.error_code
    );
}

/// How the job template under test names the job document it deploys.
enum TemplateSource {
    /// `document`: the document inline.
    Document(&'static str),
    /// `documentSource`: an S3 object URL naming it.
    DocumentSource(&'static str),
    /// `jobArn`: the job the template is based on, which the row creates first with the
    /// document and in-progress timeout the template must then carry itself.
    BasedOnJob {
        job_id: &'static str,
        thing_name: &'static str,
        document: &'static str,
        timeout_minutes: i64,
    },
}

/// One row of the job-template table: a template to create, and what a read of it must
/// report.
struct TemplateCase {
    case: &'static str,
    /// The `jobTemplateId` (modelled `length 1..64, pattern ^[a-zA-Z0-9_-]+$`).
    job_template_id: &'static str,
    /// `description`, which the model makes `@required` on this create.
    description: &'static str,
    source: TemplateSource,
    /// The optional configuration members this row's request carries. Every one of them
    /// must come back from the reader unchanged.
    configuration: Value,
}

/// The rows differ in the form and length of the id, in HOW the template names its job
/// document, and in which optional configuration members the request carries — from none
/// at all to every one the model declares.
fn template_cases() -> Vec<TemplateCase> {
    vec![
        TemplateCase {
            case: "an inline document and nothing optional",
            job_template_id: "r",
            description: "reboot the fleet",
            source: TemplateSource::Document(r#"{"operation":"reboot"}"#),
            configuration: json!({}),
        },
        TemplateCase {
            case: "an inline document with a rollout, abort and timeout configuration",
            job_template_id: "firmware_1-9",
            description: "install firmware 1.9",
            source: TemplateSource::Document(r#"{"operation":"install","version":"1.9"}"#),
            configuration: json!({
                "jobExecutionsRolloutConfig": {
                    "maximumPerMinute": 25,
                    "exponentialRate": {
                        "baseRatePerMinute": 5,
                        "incrementFactor": 2.0,
                        "rateIncreaseCriteria": { "numberOfNotifiedThings": 10 },
                    },
                },
                "abortConfig": {
                    "criteriaList": [{
                        "failureType": "FAILED",
                        "action": "CANCEL",
                        "thresholdPercentage": 20.0,
                        "minNumberOfExecutedThings": 5,
                    }],
                },
                "timeoutConfig": { "inProgressTimeoutInMinutes": 45 },
                "tags": [{ "Key": "fleet", "Value": "lasers" }],
            }),
        },
        TemplateCase {
            case: "a document source with every remaining configuration member",
            job_template_id: "DOWNLOAD-FILE_64-characters-long-id-padded-to-the-modelled-max",
            description: "download the file",
            source: TemplateSource::DocumentSource(
                "https://s3.us-east-1.amazonaws.com/example-firmware/device-firmware.1.0",
            ),
            configuration: json!({
                "presignedUrlConfig": {
                    "roleArn": "arn:aws:iam::000000000000:role/IoTJobPresignedUrlRole",
                    "expiresInSec": 1800,
                },
                "jobExecutionsRetryConfig": {
                    "criteriaList": [
                        { "failureType": "FAILED", "numberOfRetries": 2 },
                        { "failureType": "TIMED_OUT", "numberOfRetries": 1 },
                    ],
                },
                "maintenanceWindows": [
                    { "startTime": "cron(0 2 ? * SUN *)", "durationInMinutes": 120 },
                    { "startTime": "cron(0 4 ? * WED *)", "durationInMinutes": 1430 },
                ],
                "destinationPackageVersions": [
                    "arn:aws:iot:us-east-1:000000000000:package/sensor/version/1.0.0"
                ],
            }),
        },
        TemplateCase {
            case: "a template based on a job, inheriting its document and timeout",
            job_template_id: "based-on-a-job",
            description: "the rollout that worked",
            source: TemplateSource::BasedOnJob {
                job_id: "template-source-job",
                thing_name: "template-source-device",
                document: r#"{"operation":"recalibrate"}"#,
                timeout_minutes: 90,
            },
            configuration: json!({
                "abortConfig": {
                    "criteriaList": [{
                        "failureType": "ALL",
                        "action": "CANCEL",
                        "thresholdPercentage": 50.0,
                        "minNumberOfExecutedThings": 1,
                    }],
                },
            }),
        },
    ]
}

/// RULE: `DescribeJobTemplate` reports the job template registered under that
/// `jobTemplateId` from the stored record — every member its creator persisted — and a
/// template based on a job carries that job's own document and in-progress timeout.
#[tokio::test]
async fn a_created_job_template_reads_back_member_for_member() {
    let emulator = Emulator::new();

    for case in template_cases() {
        let label = case.case;
        let id = case.job_template_id;

        let mut request = case.configuration.clone();
        // What the template deploys, named the way this row names it. A row based on a
        // job creates that job first, and names nothing else: what the template must then
        // hold is the job's own document and timeout.
        let inherited: Option<(&str, i64)> = {
            let object = request
                .as_object_mut()
                .unwrap_or_else(|| panic!("case {label:?}: the row's configuration is an object"));
            object.insert("description".to_string(), json!(case.description));
            match case.source {
                TemplateSource::Document(document) => {
                    object.insert("document".to_string(), json!(document));
                    None
                }
                TemplateSource::DocumentSource(document_source) => {
                    object.insert("documentSource".to_string(), json!(document_source));
                    None
                }
                TemplateSource::BasedOnJob {
                    job_id,
                    thing_name,
                    document,
                    timeout_minutes,
                } => {
                    let created = emulator
                        .create_job(
                            job_id,
                            &json!({
                                "targets": [thing_arn(thing_name)],
                                "description": "the job the template is based on",
                                "document": document,
                                "timeoutConfig": {
                                    "inProgressTimeoutInMinutes": timeout_minutes,
                                },
                            }),
                        )
                        .await;
                    assert_eq!(
                        created.status, 200,
                        "case {label:?}: the job the template is based on must be \
                         creatable, got {} {:?}",
                        created.status, created.body
                    );
                    object.insert("jobArn".to_string(), json!(job_arn(job_id)));
                    Some((document, timeout_minutes))
                }
            }
        };
        // The members the request carried, which the reader must report back.
        let sent_members = request.as_object().cloned().unwrap_or_default();

        let created = emulator.create_job_template(id, &request).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the template must be creatable, got {} {:?}",
            created.status, created.body
        );
        assert_eq!(
            (created.text("/jobTemplateId"), created.text("/jobTemplateArn")),
            (Some(id.to_string()), Some(job_template_arn(id))),
            "case {label:?}: expected the created id and the ARN this plane mints, got {:?}",
            created.body
        );

        let read = emulator.describe_job_template(id).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: expected 200, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            (
                read.text("/jobTemplateId"),
                read.text("/jobTemplateArn"),
                read.text("/description")
            ),
            (
                Some(id.to_string()),
                Some(job_template_arn(id)),
                Some(case.description.to_string())
            ),
            "case {label:?}: expected the stored identity and description, got {:?}",
            read.body
        );
        // Every member the create carried reads back unchanged. `tags` are "metadata that
        // can be used to manage the job template" and `jobArn` names what the template was
        // based on, so neither is part of what this reader describes.
        for (member, sent) in sent_members.iter() {
            if member == "tags" || member == "jobArn" {
                assert!(
                    read.body.get(member).is_none(),
                    "case {label:?}: {member} is not part of a job template's description, \
                     got {:?}",
                    read.body
                );
                continue;
            }
            assert_eq!(
                read.body.get(member),
                Some(sent),
                "case {label:?}: expected {member} to read back as {sent}, got {:?}",
                read.body
            );
        }
        if let Some((document, timeout_minutes)) = inherited {
            assert_eq!(
                read.text("/document").as_deref(),
                Some(document),
                "case {label:?}: expected the based-on job's document, got {:?}",
                read.body
            );
            assert_eq!(
                read.body
                    .pointer("/timeoutConfig/inProgressTimeoutInMinutes")
                    .and_then(Value::as_i64),
                Some(timeout_minutes),
                "case {label:?}: expected the based-on job's in-progress timeout, got {:?}",
                read.body
            );
        }
        // `createdAt` is "The time, in seconds since the epoch, when the job template was
        // created", so the reader states it rather than leaving it out.
        assert!(
            read.body
                .get("createdAt")
                .and_then(Value::as_f64)
                .is_some_and(|seconds| seconds > 0.0),
            "case {label:?}: expected a createdAt in seconds since the epoch, got {:?}",
            read.body
        );
    }
}

/// RULE: the id is the job template's identity, so a request the store cannot take a
/// template for is refused with the class that states why and writes nothing, and an id
/// the account holds no template under is the modelled `ResourceNotFoundException`
/// (HTTP 404).
#[tokio::test]
async fn a_job_template_request_the_store_cannot_take_is_refused() {
    let emulator = Emulator::new();

    let taken = "taken-id";
    let first = emulator
        .create_job_template(
            taken,
            &json!({
                "description": "the template that took the id",
                "document": r#"{"operation":"reboot"}"#,
            }),
        )
        .await;
    assert_eq!(
        first.status, 200,
        "the first template must be creatable, got {} {:?}",
        first.status, first.body
    );

    // (case, the id the request addresses, the request, the status, the error type and
    // the message this service states that refusal with)
    let rows: Vec<(&str, &str, Value, u16, &str, String)> = vec![
        (
            "an id a template already holds",
            taken,
            json!({
                "description": "a second template under the same id",
                "document": r#"{"operation":"install"}"#,
            }),
            409,
            "ConflictException",
            format!("Job template {taken} already exists."),
        ),
        (
            "a request naming no job document at all",
            "no-document",
            json!({ "description": "a template that deploys nothing" }),
            400,
            "InvalidRequestException",
            "One of document, documentSource or jobArn is required".to_string(),
        ),
        (
            "a jobArn that is no IoT job ARN",
            "malformed-job-arn",
            json!({
                "description": "a template based on something that is not a job",
                "jobArn": thing_arn("not-a-job"),
            }),
            400,
            "InvalidRequestException",
            "Job ARN not in proper syntax.".to_string(),
        ),
        (
            "a jobArn naming no job",
            "absent-job",
            json!({
                "description": "a template based on a job nobody created",
                "jobArn": job_arn("never-created"),
            }),
            404,
            "ResourceNotFoundException",
            "Job never-created cannot be found.".to_string(),
        ),
    ];

    for (case, id, request, status, error_type, message) in rows {
        let reply = emulator.create_job_template(id, &request).await;
        assert_eq!(
            reply.status, status,
            "case {case:?}: expected {status}, got {} {:?}",
            reply.status, reply.body
        );
        assert!(
            reply
                .error_code
                .as_deref()
                .is_some_and(|code| code.starts_with(error_type)),
            "case {case:?}: expected the error type {error_type}, got {:?}",
            reply.error_code
        );
        assert_eq!(
            reply.message(),
            message,
            "case {case:?}: expected the refusal to state {message:?}, got {:?}",
            reply.body
        );
    }

    // A refused create wrote nothing: the taken id still names the FIRST template, and
    // no id a refused request named exists at all — nor does one no request ever used.
    let read = emulator.describe_job_template(taken).await;
    assert_eq!(
        (read.status, read.text("/description")),
        (200, Some("the template that took the id".to_string())),
        "the refused create must leave the first template as it was, got {:?}",
        read.body
    );
    // The ids each refused create named, the taken id in another case and with another
    // separator, and one at the modelled length bound: none of them names a template.
    let absent_ids: Vec<String> = vec![
        "no-document".to_string(),
        "malformed-job-arn".to_string(),
        "absent-job".to_string(),
        "TAKEN-ID".to_string(),
        "taken_id".to_string(),
        "t".repeat(64),
    ];
    for absent in &absent_ids {
        let reply = emulator.describe_job_template(absent).await;
        assert_eq!(
            reply.status, 404,
            "case {absent:?}: expected 404, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {absent:?}: expected ResourceNotFoundException, got {:?}",
            reply.error_code
        );
        assert_eq!(
            reply.message(),
            format!("Job template {absent} cannot be found."),
            "case {absent:?}: expected the refusal to name the template, got {:?}",
            reply.body
        );
    }
}
