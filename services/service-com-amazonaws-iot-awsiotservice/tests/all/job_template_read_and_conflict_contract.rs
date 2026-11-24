// SPDX-License-Identifier: CC0-1.0

//! What a READ of one job template always reports, and what the create of a template
//! under a taken id refuses with, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `PUT /job-templates/{jobTemplateId}` (`CreateJobTemplate`), `GET
//! /job-templates/{jobTemplateId}` (`DescribeJobTemplate`), `POST /things/{thingName}`
//! and `PUT /jobs/{jobId}` (`CreateJob`), signed for the `iot` signing name — so what is
//! asserted is what a client reads back through the modelled surface.
//!
//! Two rules are pinned here.
//!
//! 1. A reader of a job template MATERIALISES every configuration container all of whose
//!    own modelled members are optional — `presignedUrlConfig` (roleArn, expiresInSec),
//!    `jobExecutionsRolloutConfig` (maximumPerMinute, exponentialRate) and `timeoutConfig`
//!    (inProgressTimeoutInMinutes), from
//!    `smithy_query(query="operation",operation="DescribeJobTemplate")` — so the container
//!    is present for every template the account holds, carrying the values the record holds
//!    and NO member where it holds none; a container is never pruned because its contents
//!    are unset. A typed client therefore reads an empty structure rather than a null one
//!    and a describe -> recreate round trip keeps the sections the template has.
//!
//! 2. The create of a resource of the JOBS plane under an id the plane already holds is
//!    refused with that plane's fixed sentence `<Family> <id> already exists.`, with the
//!    family's own noun — two capitalised words for a job template — and the refusal
//!    carries the sentence ALONE: `ConflictException$resourceId` is left unset, because
//!    nothing other than the id the request itself carried identifies the record that
//!    already exists.
//!
//! The rows span what each rule generalises over: for the read, which containers the
//! create carried, whether a container was sent empty, each published bound of a
//! container's own members and a nested container of its own; for the refusal, the form
//! and the length of the id within `JobTemplateId`'s modelled pattern
//! (`^[a-zA-Z0-9_-]+$`, length 1..64) and the two nouns of the same plane.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "eu-west-1";

/// The configuration containers of a job-template read whose own members the model all
/// makes optional, from
/// `smithy_query(query="operation",operation="DescribeJobTemplate")`.
const TEMPLATE_CONTAINERS: [&str; 3] = [
    "jobExecutionsRolloutConfig",
    "presignedUrlConfig",
    "timeoutConfig",
];

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
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when the body carries none.
    fn text(&self, name: &str) -> Option<&str> {
        self.body.get(name).and_then(Value::as_str)
    }

    /// The `message` the body carries, or `""` when it carries none.
    fn message(&self) -> &str {
        self.text("message").unwrap_or_default()
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it,
/// reached the way the production router reaches it.
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

    async fn create_thing(&self, thing_name: &str) -> Reply {
        self.call("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await
    }

    async fn create_job(&self, job_id: &str, request: &Value) -> Reply {
        self.call("PUT", &format!("/jobs/{job_id}"), Some(request))
            .await
    }
}

/// The job-template ARN this plane mints for `job_template_id`: the `jobtemplate/`
/// resource type, in the region the request is signed for.
fn job_template_arn(job_template_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:jobtemplate/{job_template_id}")
}

/// One row of the read table: the configuration the create carries, and therefore what
/// each container must read back as.
struct ReadCase {
    case: &'static str,
    job_template_id: &'static str,
    /// The optional configuration members the create request carries, as an object.
    configuration: Value,
}

/// An IAM role ARN of the form `PresignedUrlConfig$roleArn` admits (`RoleArn`, length
/// 20..2048).
fn presigned_url_role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT_ID}:role/IoTJobPresignedUrlRole")
}

/// The rows run from a template created with no container at all to one carrying every
/// container with every member, including each published bound of a container's own
/// members (`PresignedUrlConfig$expiresInSec` range 60..3600,
/// `JobExecutionsRolloutConfig$maximumPerMinute` range 1.., and
/// `TimeoutConfig$inProgressTimeoutInMinutes`, documented as "anywhere between 1 minute
/// and 7 days (1 to 10080 minutes)") and a container nested inside another.
fn read_cases() -> Vec<ReadCase> {
    vec![
        ReadCase {
            case: "no container at all",
            job_template_id: "no-container",
            configuration: json!({}),
        },
        ReadCase {
            case: "every container sent EMPTY by the client",
            job_template_id: "empty-containers",
            configuration: json!({
                "presignedUrlConfig": {},
                "jobExecutionsRolloutConfig": {},
                "timeoutConfig": {},
            }),
        },
        ReadCase {
            case: "a pre-signed URL configuration at the lowest published expiry",
            job_template_id: "presigned-only_60",
            configuration: json!({
                "presignedUrlConfig": {
                    "roleArn": presigned_url_role_arn(),
                    "expiresInSec": 60,
                },
            }),
        },
        ReadCase {
            case: "a pre-signed URL configuration at the highest published expiry",
            job_template_id: "presigned-only-3600",
            configuration: json!({
                "presignedUrlConfig": {
                    "roleArn": presigned_url_role_arn(),
                    "expiresInSec": 3600,
                },
            }),
        },
        ReadCase {
            case: "a staged rollout at the lowest published rate",
            job_template_id: "rollout-only",
            configuration: json!({ "jobExecutionsRolloutConfig": { "maximumPerMinute": 1 } }),
        },
        ReadCase {
            case: "a staged rollout whose exponential rate carries criteria of its own",
            job_template_id: "Exponential_Rollout",
            configuration: json!({
                "jobExecutionsRolloutConfig": {
                    "exponentialRate": {
                        "baseRatePerMinute": 5,
                        "incrementFactor": 1.5,
                        "rateIncreaseCriteria": { "numberOfSucceededThings": 2 },
                    },
                },
            }),
        },
        ReadCase {
            case: "a timeout at the longest documented interval",
            job_template_id: "timeout-only-10080",
            configuration: json!({ "timeoutConfig": { "inProgressTimeoutInMinutes": 10080 } }),
        },
        ReadCase {
            case: "every container carrying every member",
            job_template_id: "every-container",
            configuration: json!({
                "presignedUrlConfig": {
                    "roleArn": presigned_url_role_arn(),
                    "expiresInSec": 900,
                },
                "jobExecutionsRolloutConfig": {
                    "maximumPerMinute": 10,
                    "exponentialRate": {
                        "baseRatePerMinute": 1,
                        "incrementFactor": 2.5,
                        "rateIncreaseCriteria": { "numberOfNotifiedThings": 10 },
                    },
                },
                "timeoutConfig": { "inProgressTimeoutInMinutes": 1 },
            }),
        },
    ]
}

/// RULE 1: `DescribeJobTemplate` reports every configuration container whose own members
/// are all optional for EVERY template the account holds — carrying the values the record
/// holds, and no member where it holds none — so a container is never pruned because its
/// contents are unset.
#[tokio::test]
async fn a_job_template_read_materialises_every_optional_container() {
    let emulator = Emulator::new();

    for case in read_cases() {
        let label = case.case;
        let id = case.job_template_id;
        let description = format!("the template of case {label}");
        let document = r#"{"operation":"reboot"}"#;

        let mut request = case.configuration.clone();
        {
            let object = request
                .as_object_mut()
                .unwrap_or_else(|| panic!("case {label:?}: the row's configuration is an object"));
            object.insert("description".to_string(), json!(description));
            object.insert("document".to_string(), json!(document));
        }
        let created = emulator.create_job_template(id, &request).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the template must be creatable, got {} {:?}",
            created.status, created.body
        );

        let read = emulator.describe_job_template(id).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: expected 200, got {} {:?}",
            read.status, read.body
        );
        // The record round-trips through the reader: the identity the create minted and
        // what the template deploys.
        assert_eq!(
            (
                read.text("jobTemplateId"),
                read.text("jobTemplateArn"),
                read.text("description"),
                read.text("document"),
            ),
            (
                Some(id),
                Some(job_template_arn(id).as_str()),
                Some(description.as_str()),
                Some(document),
            ),
            "case {label:?}: expected the created template to read back, got {:?}",
            read.body
        );

        for container in TEMPLATE_CONTAINERS {
            // A container the create did not carry — or carried empty — reads back as the
            // EMPTY structure, never as an absent key.
            let expected = case
                .configuration
                .get(container)
                .cloned()
                .unwrap_or_else(|| json!({}));
            let actual = read.body.get(container);
            assert!(
                actual.is_some_and(Value::is_object),
                "case {label:?}: expected {container} to be reported as a structure, got \
                 {actual:?}"
            );
            assert_eq!(
                actual,
                Some(&expected),
                "case {label:?}: expected {container} to read back as {expected}, got \
                 {actual:?}"
            );
        }
    }
}

/// Identifiers of the form `^[a-zA-Z0-9_-]+$` within 1..64 — the `JobTemplateId` /
/// `JobId` constraint — differing in length, case and separator.
fn plane_ids() -> Vec<String> {
    vec![
        "x".to_string(),
        "nightly-reboot".to_string(),
        "Reboot_42".to_string(),
        "i".repeat(64),
    ]
}

/// RULE 2: a create of a resource of the jobs plane under an id the plane already holds
/// is refused with that plane's sentence in the family's own noun, and the refusal
/// carries that sentence alone — no member naming the record that already exists — while
/// nothing of the stored record changes.
#[tokio::test]
async fn a_taken_job_template_id_is_refused_with_the_family_sentence_alone() {
    for id in plane_ids() {
        // A fresh emulator per row: a refusal must depend only on what this row created.
        let emulator = Emulator::new();

        let created = emulator
            .create_job_template(
                &id,
                &json!({
                    "description": "the template that took the id",
                    "document": r#"{"operation":"reboot"}"#,
                }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {id:?}: the first template must be creatable, got {} {:?}",
            created.status, created.body
        );

        let refused = emulator
            .create_job_template(
                &id,
                &json!({
                    "description": "a second template under the same id",
                    "document": r#"{"operation":"install"}"#,
                    "timeoutConfig": { "inProgressTimeoutInMinutes": 30 },
                }),
            )
            .await;
        assert_eq!(
            refused.status, 409,
            "case {id:?}: expected status 409, got {} {:?}",
            refused.status, refused.body
        );
        assert!(
            refused
                .error_type
                .as_deref()
                .is_some_and(|code| code.starts_with("ConflictException")),
            "case {id:?}: expected the ConflictException shape, got {:?}",
            refused.error_type
        );
        assert_eq!(
            refused.message(),
            format!("Job Template {id} already exists."),
            "case {id:?}: expected the jobs plane's sentence with the job-template noun, \
             got {:?}",
            refused.body
        );
        // The sentence ALONE: nothing other than the id the request carried identifies
        // the record that already exists, so the modelled `resourceId` is left unset.
        assert_eq!(
            refused.body.get("resourceId"),
            None,
            "case {id:?}: this refusal reports no resourceId, got {:?}",
            refused.body
        );
        // Every MODELLED member of the shape, beside the `__type` discriminator the wire
        // protocol carries for the error shape itself.
        let reported: Vec<&str> = refused
            .body
            .as_object()
            .map(|body| {
                body.keys()
                    .map(String::as_str)
                    .filter(|member| *member != "__type")
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            reported,
            vec!["message"],
            "case {id:?}: the refusal body carries the message alone, got {:?}",
            refused.body
        );

        // The refused create wrote nothing: the id still names the FIRST template, with
        // the containers a read always reports.
        let read = emulator.describe_job_template(&id).await;
        assert_eq!(
            (read.status, read.text("description")),
            (200, Some("the template that took the id")),
            "case {id:?}: the refused create must leave the first template as it was, got \
             {:?}",
            read.body
        );
        assert_eq!(
            read.body.get("timeoutConfig"),
            Some(&json!({})),
            "case {id:?}: the refused create's timeout must not have reached the record, \
             got {:?}",
            read.body
        );
    }
}

/// The other half of rule 2: the noun in the sentence is the FAMILY's, not the plane's —
/// the same id taken by a job and by a job template is refused with two different nouns,
/// each refusal still carrying its sentence alone.
#[tokio::test]
async fn each_jobs_plane_family_states_the_collision_in_its_own_noun() {
    let emulator = Emulator::new();
    let id = "shared-id";
    let thing = "targeted";

    let thing_created = emulator.create_thing(thing).await;
    assert_eq!(
        thing_created.status, 200,
        "the targeted thing must be creatable, got {} {:?}",
        thing_created.status, thing_created.body
    );
    let job = emulator
        .create_job(
            id,
            &json!({
                "targets": [format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing}")],
                "document": "{}",
            }),
        )
        .await;
    assert_eq!(
        job.status, 200,
        "the job must be creatable, got {} {:?}",
        job.status, job.body
    );
    let template = emulator
        .create_job_template(
            id,
            &json!({ "description": "a template under the job's id", "document": "{}" }),
        )
        .await;
    assert_eq!(
        template.status, 200,
        "a template may take the id a job holds, got {} {:?}",
        template.status, template.body
    );

    for (case, call, expected) in [
        (
            "the job",
            emulator
                .create_job(
                    id,
                    &json!({
                        "targets": [format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing}")],
                        "document": "{}",
                    }),
                )
                .await,
            format!("Job {id} already exists."),
        ),
        (
            "the job template",
            emulator
                .create_job_template(
                    id,
                    &json!({ "description": "a second attempt", "document": "{}" }),
                )
                .await,
            format!("Job Template {id} already exists."),
        ),
    ] {
        assert_eq!(
            call.status, 409,
            "case {case:?}: expected status 409, got {} {:?}",
            call.status, call.body
        );
        assert_eq!(
            call.message(),
            expected,
            "case {case:?}: expected the sentence {expected:?}, got {:?}",
            call.body
        );
        assert_eq!(
            call.body.get("resourceId"),
            None,
            "case {case:?}: this plane reports no resourceId, got {:?}",
            call.body
        );
    }
}
