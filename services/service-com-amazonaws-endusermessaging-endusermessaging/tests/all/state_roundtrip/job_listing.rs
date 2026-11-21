// SPDX-License-Identifier: CC0-1.0

//! `ListJobs`: the asynchronous jobs this service holds, reported and filtered over
//! the wire.
//!
//! Every assertion goes through `MultiServiceRouter::dispatch`, so it exercises the
//! same restJson1 path a real SDK client takes: routing, deserialization, boundary
//! constraint validation, the handler, and error rendering. The jobs themselves are
//! written into the service's own store the way an asynchronous operation of this
//! service writes one — `mint_job_id` then `record_job` — because every operation that
//! starts a job is still a scaffold, so no call can create one yet.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * a stored job is reported member for member — for every `operationType` this
//!   service runs jobs for, every `JobStatus`, with and without a `brandProfileId`,
//!   with and without the error members a failed job carries, and for every number of
//!   created resources, including none;
//! * the filters hold together: a job is reported exactly when it matches every filter
//!   the request carries, for every combination of `status`, `operationType` and
//!   `brandProfileId` — the last resolving a profile by its bare id or by its ARN;
//! * an `operationType` that names no asynchronous operation of this service is
//!   `ValidationException` (400), whatever a request would otherwise have matched,
//!   while every one that does is served;
//! * a `brandProfileId` that is neither an identifier nor an ARN of a brand profile is
//!   `ValidationException` (400), while a well-formed identifier that names no stored
//!   profile selects nothing;
//! * every matching job is returned exactly once across pages, in one order, for every
//!   page size, and a token is emitted only while jobs remain; a token this service
//!   never issued is `ValidationException` (400);
//! * a job accepted in `PROCESSING` reaches its terminal status when the clock makes it
//!   due, reports the resources it created from then on, and is reported as updated at
//!   the instant that was owed.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_endusermessaging_endusermessaging::ops::types::JobResource;
use service_com_amazonaws_endusermessaging_endusermessaging::state::{
    service_state, Job, JobSettlement, JOB_OPERATION_TYPE_VALUES, JOB_SETTLEMENT_DELAY,
};
use service_com_amazonaws_endusermessaging_endusermessaging::EmulatedService;

/// This service's sigv4 signing name, from the model (`OPERATIONS.md`, "Wire protocol").
const SIGNING_NAME: &str = "end-user-messaging";

/// The Region every request of this suite is signed for.
const REGION: &str = "us-east-1";

/// The account id every generated emulator presents, as `engine` defines it.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// How long to keep looking for a job's terminal status before giving up: generous,
/// because the condition is the clock reaching the instant the transition was owed at,
/// not a bound on how fast the host serves a request.
const SETTLEMENT_DEADLINE: Duration = Duration::from_secs(120);

/// How long to wait between looks while waiting for a job to reach its terminal status.
const SETTLEMENT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One response, reduced to what the assertions read.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The wire error code (`x-amzn-errortype`, else the body's `__type`), with any
    /// `namespace#` prefix stripped. `None` on success.
    error: Option<String>,
    body: Value,
}

/// One emulator over one shared engine: every call, and every seeded job, sees the
/// same store.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    async fn call(&self, method: &str, path_and_query: &str, body: Option<Value>) -> Reply {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/{SIGNING_NAME}/aws4_request, \
             SignedHeaders=host, Signature=00"
        );
        let payload = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("a serializable body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(format!(
                "https://{SIGNING_NAME}.{REGION}.amazonaws.com{path_and_query}"
            ))
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .body(payload)
            .expect("a well-formed test request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_error = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        let error = if status < 400 {
            None
        } else {
            header_error
                .or_else(|| body["__type"].as_str().map(str::to_string))
                .map(|code| {
                    code.rsplit('#')
                        .next()
                        .unwrap_or(code.as_str())
                        .split('/')
                        .next()
                        .unwrap_or(code.as_str())
                        .to_string()
                })
        };
        Reply {
            status,
            error,
            body,
        }
    }

    /// `ListJobs`, with `query` as the request's whole query string.
    async fn list_jobs(&self, query: &str) -> Reply {
        self.call("GET", &format!("/v1/jobs{query}"), None).await
    }

    /// `ListBrandProfiles`, so a token another paginated operation of this service
    /// issued can be offered to `ListJobs`.
    async fn list_profiles(&self, query: &str) -> Reply {
        self.call("GET", &format!("/v1/brand-profiles{query}"), None)
            .await
    }

    /// `CreateBrandProfile`, for the profiles the seeded jobs operate on.
    async fn create_profile(&self, name: &str) -> (String, String) {
        let created = self
            .call(
                "POST",
                "/v1/brand-profiles",
                Some(json!({ "brandProfileName": name })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "creating the profile {name:?} the jobs operate on must succeed, got {} / {:?}",
            created.status, created.body
        );
        let id = created.body["brandProfileId"]
            .as_str()
            .unwrap_or_else(|| panic!("a created profile reports its id, got {:?}", created.body))
            .to_string();
        let arn = created.body["brandProfileArn"]
            .as_str()
            .unwrap_or_else(|| panic!("a created profile reports its ARN, got {:?}", created.body))
            .to_string();
        (id, arn)
    }

    /// Store one job the way an asynchronous operation of this service does: mint an
    /// id, then record the job with the terminal status the clock owes it.
    fn seed_job(
        &self,
        operation_type: &str,
        brand_profile_id: Option<&str>,
        created_at: SystemTime,
        settlement: JobSettlement,
    ) -> String {
        let state = service_state(&*self.engine).expect("this service's own store");
        let mut store = state.lock();
        let job_id = store.mint_job_id();
        store.record_job(Job::accepted(
            &job_id,
            operation_type,
            brand_profile_id,
            created_at,
            settlement,
        ));
        job_id
    }
}

/// One seeded job, and what every report of it must say.
#[derive(Debug, Clone)]
struct Seeded {
    case: String,
    job_id: String,
    status: String,
    operation_type: String,
    brand_profile_id: Option<String>,
    error_code: Option<String>,
    error_message: Option<String>,
    /// The `resources` member as the wire must carry it: absent when the job created
    /// nothing, since `JobResourceList` is modelled `@length(1..10)`.
    resources: Value,
    created_at_secs: u64,
    updated_at_secs: u64,
}

/// The whole epoch second `at` falls in, which is the resolution this service reports a
/// `Timestamp` at.
fn epoch_secs(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .expect("an instant after the epoch")
        .as_secs()
}

/// A `JobResource` as the wire carries it.
fn resource_body(resource: &JobResource) -> Value {
    json!({
        "resourceType": resource.resource_type,
        "resourceId": resource.resource_id,
        "resourceArn": resource.resource_arn,
    })
}

/// The brand-profile resource a job that created or refreshed a profile reports.
fn brand_profile_resource(id: &str, arn: &str) -> JobResource {
    JobResource {
        // One of `JobResourceType`'s modelled values
        // (`crate::ops::types::JOB_RESOURCE_TYPE_VALUES`).
        resource_type: "BRAND_PROFILE".to_string(),
        resource_id: id.to_string(),
        resource_arn: arn.to_string(),
    }
}

/// The registration resource a job that created or refreshed a registration reports.
fn registration_resource(ordinal: usize) -> JobResource {
    let id = format!("R-{ordinal:017}");
    JobResource {
        resource_type: "REGISTRATION".to_string(),
        resource_id: id.clone(),
        resource_arn: format!("arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:registration/{id}"),
    }
}

/// The jobs a store is seeded with, spanning the classes a report has to carry: every
/// operation type this service runs jobs for, every status, a job that operates on a
/// profile and one that creates one, the error members a failed job carries, and
/// zero, one and several created resources.
///
/// A job whose settlement came due in the past is already terminal by the time any
/// call reads it; the one still `PROCESSING` is due far enough ahead that no read of
/// this suite can settle it, so the table is the same whatever order the cases run in
/// and however slowly the host runs them.
async fn seeded_jobs(em: &Emulator) -> (Vec<Seeded>, (String, String), (String, String)) {
    let first = em.create_profile("jobs-first").await;
    let second = em.create_profile("jobs-second").await;

    let now = SystemTime::now();
    let created_at = now - Duration::from_secs(600);
    let settled_at = now - Duration::from_secs(300);
    let far_ahead = now + Duration::from_secs(3_600);

    let mut rows: Vec<Seeded> = Vec::new();

    // A job that CREATED a brand profile: `brandProfileId` is absent, as
    // `JobSummary$brandProfileId` documents for an operation that creates one.
    let resources = vec![brand_profile_resource(&first.0, &first.1)];
    rows.push(Seeded {
        case: "a succeeded create-from-registration job, no brand profile".to_string(),
        job_id: em.seed_job(
            "CreateBrandProfileFromRegistration",
            None,
            created_at,
            JobSettlement::succeeds_at(settled_at, resources.clone()),
        ),
        status: "SUCCESS".to_string(),
        operation_type: "CreateBrandProfileFromRegistration".to_string(),
        brand_profile_id: None,
        error_code: None,
        error_message: None,
        resources: Value::Array(resources.iter().map(resource_body).collect()),
        created_at_secs: epoch_secs(created_at),
        updated_at_secs: epoch_secs(settled_at),
    });

    // Several created resources, on the first profile.
    let resources = vec![registration_resource(1), registration_resource(2)];
    rows.push(Seeded {
        case: "a succeeded create-registrations job with two resources".to_string(),
        job_id: em.seed_job(
            "CreateRegistrationsFromBrandProfile",
            Some(&first.0),
            created_at,
            JobSettlement::succeeds_at(settled_at, resources.clone()),
        ),
        status: "SUCCESS".to_string(),
        operation_type: "CreateRegistrationsFromBrandProfile".to_string(),
        brand_profile_id: Some(first.0.clone()),
        error_code: None,
        error_message: None,
        resources: Value::Array(resources.iter().map(resource_body).collect()),
        created_at_secs: epoch_secs(created_at),
        updated_at_secs: epoch_secs(settled_at),
    });

    // A failed job: the error members a failed job carries, and no resources.
    rows.push(Seeded {
        case: "a failed update-from-registration job".to_string(),
        job_id: em.seed_job(
            "UpdateBrandProfileFromRegistration",
            Some(&first.0),
            created_at,
            JobSettlement::fails_at(
                settled_at,
                "RegistrationNotFound",
                "The registration this job imports from no longer exists.",
            ),
        ),
        status: "FAILED".to_string(),
        operation_type: "UpdateBrandProfileFromRegistration".to_string(),
        brand_profile_id: Some(first.0.clone()),
        error_code: Some("RegistrationNotFound".to_string()),
        error_message: Some("The registration this job imports from no longer exists.".to_string()),
        resources: Value::Null,
        created_at_secs: epoch_secs(created_at),
        updated_at_secs: epoch_secs(settled_at),
    });

    // One created resource, on the second profile.
    let resources = vec![registration_resource(3)];
    rows.push(Seeded {
        case: "a succeeded create-registrations job with one resource".to_string(),
        job_id: em.seed_job(
            "CreateRegistrationsFromBrandProfile",
            Some(&second.0),
            created_at,
            JobSettlement::succeeds_at(settled_at, resources.clone()),
        ),
        status: "SUCCESS".to_string(),
        operation_type: "CreateRegistrationsFromBrandProfile".to_string(),
        brand_profile_id: Some(second.0.clone()),
        error_code: None,
        error_message: None,
        resources: Value::Array(resources.iter().map(resource_body).collect()),
        created_at_secs: epoch_secs(created_at),
        updated_at_secs: epoch_secs(settled_at),
    });

    // Still running: no resources and no error members yet.
    rows.push(Seeded {
        case: "a processing update-registrations job".to_string(),
        job_id: em.seed_job(
            "UpdateRegistrationsFromBrandProfile",
            Some(&second.0),
            created_at,
            JobSettlement::succeeds_at(far_ahead, vec![registration_resource(4)]),
        ),
        status: "PROCESSING".to_string(),
        operation_type: "UpdateRegistrationsFromBrandProfile".to_string(),
        brand_profile_id: Some(second.0.clone()),
        error_code: None,
        error_message: None,
        resources: Value::Null,
        created_at_secs: epoch_secs(created_at),
        updated_at_secs: epoch_secs(created_at),
    });

    // Still running, and creating a profile, so it names none.
    rows.push(Seeded {
        case: "a processing create-from-registration job, no brand profile".to_string(),
        job_id: em.seed_job(
            "CreateBrandProfileFromRegistration",
            None,
            created_at,
            JobSettlement::succeeds_at(far_ahead, Vec::new()),
        ),
        status: "PROCESSING".to_string(),
        operation_type: "CreateBrandProfileFromRegistration".to_string(),
        brand_profile_id: None,
        error_code: None,
        error_message: None,
        resources: Value::Null,
        created_at_secs: epoch_secs(created_at),
        updated_at_secs: epoch_secs(created_at),
    });

    (rows, first, second)
}

/// The `jobs` a reply reports.
fn listed_jobs(case: &str, reply: &Reply) -> Vec<Value> {
    assert_eq!(
        reply.status, 200,
        "case {case}: ListJobs must succeed, got {} / {:?}",
        reply.status, reply.body
    );
    reply.body["jobs"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: jobs is not a list in {:?}", reply.body))
        .clone()
}

/// The `jobId`s a reply reports, in the order it reported them.
fn listed_ids(case: &str, reply: &Reply) -> Vec<String> {
    listed_jobs(case, reply)
        .iter()
        .map(|job| {
            job["jobId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a listed job has no jobId: {job:?}"))
                .to_string()
        })
        .collect()
}

/// The number a reply reports under `member` of a listed job.
fn member_secs(case: &str, job: &Value, member: &str) -> u64 {
    let reported = job[member]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case}: {member} is not a number in {job:?}"));
    assert!(
        reported >= 0.0 && reported.fract() == 0.0,
        "case {case}: {member} must be a whole count of epoch seconds, got {reported}"
    );
    reported as u64
}

#[tokio::test]
async fn every_stored_job_is_reported_member_for_member() {
    let em = Emulator::new();
    let (seeded, _, _) = seeded_jobs(&em).await;

    let reply = em.list_jobs("").await;
    let listed = listed_jobs("every seeded job", &reply);
    assert_eq!(
        listed.len(),
        seeded.len(),
        "case \"every seeded job\": expected {} jobs, got {:?}",
        seeded.len(),
        reply.body
    );

    for row in &seeded {
        let case = &row.case;
        let reported = listed
            .iter()
            .find(|job| job["jobId"].as_str() == Some(row.job_id.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: the job {} must be reported, got {:?}",
                    row.job_id, reply.body
                )
            });
        assert_eq!(
            reported["status"].as_str(),
            Some(row.status.as_str()),
            "case {case}: expected status {:?}, got {reported:?}",
            row.status
        );
        assert_eq!(
            reported["operationType"].as_str(),
            Some(row.operation_type.as_str()),
            "case {case}: expected operationType {:?}, got {reported:?}",
            row.operation_type
        );
        assert_eq!(
            reported["brandProfileId"].as_str(),
            row.brand_profile_id.as_deref(),
            "case {case}: expected brandProfileId {:?}, got {reported:?}",
            row.brand_profile_id
        );
        assert_eq!(
            reported["errorCode"].as_str(),
            row.error_code.as_deref(),
            "case {case}: expected errorCode {:?}, got {reported:?}",
            row.error_code
        );
        assert_eq!(
            reported["errorMessage"].as_str(),
            row.error_message.as_deref(),
            "case {case}: expected errorMessage {:?}, got {reported:?}",
            row.error_message
        );
        assert_eq!(
            reported["resources"], row.resources,
            "case {case}: expected resources {:?}, got {reported:?}",
            row.resources
        );
        assert_eq!(
            member_secs(case, reported, "createdAt"),
            row.created_at_secs,
            "case {case}: expected createdAt {}, got {reported:?}",
            row.created_at_secs
        );
        assert_eq!(
            member_secs(case, reported, "updatedAt"),
            row.updated_at_secs,
            "case {case}: a job is reported as updated when its status came due; \
             expected {}, got {reported:?}",
            row.updated_at_secs
        );
    }

    // Nothing left to page through, so no continuation token.
    assert_eq!(
        reply.body["nextToken"],
        Value::Null,
        "case \"every seeded job\": one page holds every job, so no token is issued, \
         got {:?}",
        reply.body
    );
}

#[tokio::test]
async fn an_empty_store_reports_no_jobs() {
    let em = Emulator::new();
    let reply = em.list_jobs("").await;
    assert_eq!(
        listed_jobs("no jobs stored", &reply).len(),
        0,
        "case \"no jobs stored\": expected an empty list, got {:?}",
        reply.body
    );
    assert_eq!(
        reply.body["nextToken"],
        Value::Null,
        "case \"no jobs stored\": nothing remains, so no token is issued, got {:?}",
        reply.body
    );
}

#[tokio::test]
async fn a_job_is_reported_exactly_when_it_matches_every_filter() {
    let em = Emulator::new();
    let (seeded, first, second) = seeded_jobs(&em).await;

    // A well-formed identifier of the documented form that no stored profile carries:
    // `bp-` and 17 alphanumerics, as `BrandProfileIdOrArn` documents the bare id.
    let unknown_profile = format!("bp-{}", "0".repeat(17));

    // Each row is a candidate value for one filter, with the job member it selects on.
    let statuses: [Option<&str>; 4] = [None, Some("SUCCESS"), Some("FAILED"), Some("PROCESSING")];
    let mut operation_types: Vec<Option<&str>> = vec![None];
    operation_types.extend(JOB_OPERATION_TYPE_VALUES.iter().map(|value| Some(*value)));
    // The `brandProfileId` filter takes either form, so a profile appears as its bare
    // id and as its ARN, and both must select the same jobs.
    let profiles: [(&str, Option<&str>, Option<&str>); 5] = [
        ("no brand profile filter", None, None),
        ("the first profile by id", Some(&first.0), Some(&first.0)),
        ("the first profile by ARN", Some(&first.1), Some(&first.0)),
        ("the second profile by id", Some(&second.0), Some(&second.0)),
        (
            "a well-formed id no profile carries",
            Some(&unknown_profile),
            Some(&unknown_profile),
        ),
    ];

    for status in statuses {
        for operation_type in &operation_types {
            for (profile_case, filter, selects) in profiles {
                let mut query = String::new();
                let mut push = |name: &str, value: &str| {
                    query.push(if query.is_empty() { '?' } else { '&' });
                    query.push_str(name);
                    query.push('=');
                    query.push_str(value);
                };
                if let Some(status) = status {
                    push("status", status);
                }
                if let Some(operation_type) = operation_type {
                    push("operationType", operation_type);
                }
                if let Some(filter) = filter {
                    push("brandProfileId", filter);
                }
                let case = format!(
                    "status={status:?} operationType={operation_type:?} {profile_case}",
                );

                let expected: Vec<String> = seeded
                    .iter()
                    .filter(|row| {
                        status.is_none_or(|status| row.status == status)
                            && operation_type
                                .is_none_or(|requested| row.operation_type == requested)
                            && selects.is_none_or(|requested| {
                                row.brand_profile_id.as_deref() == Some(requested)
                            })
                    })
                    .map(|row| row.job_id.clone())
                    .collect();

                let reply = em.list_jobs(&query).await;
                let reported = listed_ids(&case, &reply);
                assert_eq!(
                    reported.iter().collect::<BTreeSet<&String>>(),
                    expected.iter().collect::<BTreeSet<&String>>(),
                    "case {case}: expected the jobs matching every filter ({expected:?}), \
                     got {reported:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn an_operation_type_that_starts_no_job_of_this_service_is_refused() {
    let em = Emulator::new();
    let (seeded, _, _) = seeded_jobs(&em).await;

    // Each row satisfies `JobOperationType`'s modelled `@length(1..128)` — so the
    // boundary validator admits it — and names no asynchronous operation of this
    // service.
    let longest = "A".repeat(128);
    let refused: [(&str, &str); 6] = [
        (
            "a mutating operation of this service that starts no job",
            "CreateBrandProfile",
        ),
        (
            "a job operation type in another case",
            "createbrandprofilefromregistration",
        ),
        (
            "a job operation type with a suffix",
            "CreateBrandProfileFromRegistrationV2",
        ),
        ("an operation of another API", "ImportRegistration"),
        ("a single character", "x"),
        ("the longest value the member admits", longest.as_str()),
    ];

    for (case, operation_type) in refused {
        let reply = em
            .list_jobs(&format!("?operationType={operation_type}"))
            .await;
        assert_eq!(
            reply.status, 400,
            "case {case}: expected 400, got {} / {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case {case}: expected ValidationException, got {:?} / {:?}",
            reply.error,
            reply.body
        );
        let message = reply.body["message"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the refusal states a message: {:?}", reply.body));
        assert!(
            message.contains("operationType") && message.contains(operation_type),
            "case {case}: the refusal must name the member and the value it refused, \
             got {message:?}"
        );
        let field = &reply.body["fieldList"][0];
        assert_eq!(
            field["path"].as_str(),
            Some("operationType"),
            "case {case}: the refusal must point at the member it refused, got {:?}",
            reply.body
        );
        let stated = field["message"].as_str().unwrap_or_else(|| {
            panic!("case {case}: the listed field states a message: {:?}", reply.body)
        });
        for permitted in JOB_OPERATION_TYPE_VALUES {
            assert!(
                stated.contains(permitted),
                "case {case}: the refusal must state the set the value had to come from, \
                 {permitted} is missing from {stated:?}"
            );
        }
    }

    // And every operation type this service does run jobs for is served, whether or
    // not any stored job carries it.
    for operation_type in JOB_OPERATION_TYPE_VALUES {
        let case = format!("operationType={operation_type}");
        let reply = em
            .list_jobs(&format!("?operationType={operation_type}"))
            .await;
        let reported = listed_ids(&case, &reply);
        let expected: Vec<String> = seeded
            .iter()
            .filter(|row| row.operation_type == *operation_type)
            .map(|row| row.job_id.clone())
            .collect();
        assert_eq!(
            reported.iter().collect::<BTreeSet<&String>>(),
            expected.iter().collect::<BTreeSet<&String>>(),
            "case {case}: expected {expected:?}, got {reported:?}"
        );
    }

    // A `status` outside `JobStatus`'s modelled value set is refused at the boundary,
    // before this handler runs.
    let reply = em.list_jobs("?status=RUNNING").await;
    assert_eq!(
        (reply.status, reply.error.as_deref()),
        (400, Some("ValidationException")),
        "case \"a status outside the modelled value set\": expected a 400 \
         ValidationException, got {:?}",
        reply.body
    );
}

#[tokio::test]
async fn a_brand_profile_filter_that_names_no_profile_at_all_is_refused() {
    let em = Emulator::new();
    let (_, first, _) = seeded_jobs(&em).await;

    // Each row satisfies `BrandProfileIdOrArn`'s modelled `@length(1..256)` and
    // `^[A-Za-z0-9_:/-]+$` pattern, and is still not an identifier of a brand profile:
    // the boundary validator admits it and this operation refuses it.
    let short_body = format!("bp-{}", "a".repeat(16));
    let non_alphanumeric_body = format!("bp-{}", "a".repeat(16) + "_");
    let other_resource = format!(
        "arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:notify-code-configuration/ncc-00000000000000001"
    );
    let other_vendor = format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:topic/jobs");
    let refused: [(&str, &str); 5] = [
        ("not an identifier of anything", "jobs-please"),
        ("an id body one character short", short_body.as_str()),
        (
            "an id body with a character outside the documented form",
            non_alphanumeric_body.as_str(),
        ),
        (
            "an ARN of another resource of this service",
            other_resource.as_str(),
        ),
        ("an ARN of another vendor", other_vendor.as_str()),
    ];

    for (case, brand_profile_id) in refused {
        let reply = em
            .list_jobs(&format!("?brandProfileId={brand_profile_id}"))
            .await;
        assert_eq!(
            (reply.status, reply.error.as_deref()),
            (400, Some("ValidationException")),
            "case {case}: expected a 400 ValidationException, got {} / {:?}",
            reply.status,
            reply.body
        );
    }

    // The same member in its ARN form, for a profile that does exist, is served.
    let case = "the ARN of a stored profile";
    let reply = em
        .list_jobs(&format!("?brandProfileId={}", first.1))
        .await;
    assert!(
        !listed_jobs(case, &reply).is_empty(),
        "case {case}: the jobs of that profile must be reported, got {:?}",
        reply.body
    );
}

#[tokio::test]
async fn every_matching_job_is_returned_exactly_once_across_pages() {
    let em = Emulator::new();
    let (seeded, _, _) = seeded_jobs(&em).await;
    let every_id: Vec<String> = listed_ids("one page", &em.list_jobs("").await);
    assert_eq!(
        every_id.len(),
        seeded.len(),
        "the whole store must be readable in one page first, got {every_id:?}"
    );

    for page_size in [1_usize, 2, 3, 5, 100] {
        let case = format!("maxResults={page_size}");
        let mut collected: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One more round than there can be pages, so a token that never clears is a
        // failed assertion rather than a loop that does not end.
        for round in 0..=seeded.len() + 1 {
            let mut query = format!("?maxResults={page_size}");
            if let Some(token) = token.as_deref() {
                query.push_str(&format!("&nextToken={token}"));
            }
            let reply = em.list_jobs(&query).await;
            let page = listed_ids(&case, &reply);
            assert!(
                page.len() <= page_size,
                "case {case} round {round}: a page holds at most maxResults jobs, got {page:?}"
            );
            assert!(
                !page.is_empty(),
                "case {case} round {round}: a page reached through a token must hold at \
                 least one job, got {:?}",
                reply.body
            );
            collected.extend(page);
            token = reply.body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
            assert!(
                collected.len() < seeded.len(),
                "case {case} round {round}: a token is issued only while jobs remain, \
                 got {:?} after {collected:?}",
                reply.body
            );
        }
        assert!(
            token.is_none(),
            "case {case}: paging must finish, got a token after {collected:?}"
        );
        assert_eq!(
            collected, every_id,
            "case {case}: every job must be returned exactly once, in one order, \
             got {collected:?}"
        );
    }

    // A token this operation never issued: a value that decodes to nothing, and one
    // another paginated operation of this service issued.
    let foreign = em.list_profiles("?maxResults=1").await;
    let foreign_token = foreign.body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "ListBrandProfiles must issue a token for the seeded profiles, got {:?}",
                foreign.body
            )
        })
        .to_string();
    for (case, token) in [
        ("a value that is not a token at all", "not-a-token"),
        ("a token of another paginated operation", &foreign_token),
    ] {
        let reply = em.list_jobs(&format!("?nextToken={token}")).await;
        assert_eq!(
            (reply.status, reply.error.as_deref()),
            (400, Some("ValidationException")),
            "case {case}: expected a 400 ValidationException, got {} / {:?}",
            reply.status,
            reply.body
        );
    }
}

#[tokio::test]
async fn a_processing_job_reaches_its_terminal_status_when_the_clock_makes_it_due() {
    let em = Emulator::new();
    let (profile_id, profile_arn) = em.create_profile("settling").await;

    let accepted_at = SystemTime::now();
    let due_at = accepted_at + JOB_SETTLEMENT_DELAY;
    let resources = vec![brand_profile_resource(&profile_id, &profile_arn)];
    let job_id = em.seed_job(
        "UpdateBrandProfileFromRegistration",
        Some(&profile_id),
        accepted_at,
        JobSettlement::succeeds_at(due_at, resources.clone()),
    );

    // Look again until the status the clock owes is reported, or a generous deadline
    // passes: the condition is the transition coming due, never a bound on how fast
    // the host serves a request.
    let give_up_at = Instant::now() + SETTLEMENT_DEADLINE;
    let settled = loop {
        let reply = em.list_jobs("").await;
        let listed = listed_jobs("a settling job", &reply);
        assert_eq!(
            listed.len(),
            1,
            "case \"a settling job\": reading a job must not change which jobs a later \
             caller sees, got {:?}",
            reply.body
        );
        let job = listed[0].clone();
        assert_eq!(
            job["jobId"].as_str(),
            Some(job_id.as_str()),
            "case \"a settling job\": the seeded job must be the one reported, got {job:?}"
        );
        match job["status"].as_str() {
            Some("PROCESSING") => {
                assert_eq!(
                    job["resources"],
                    Value::Null,
                    "case \"a settling job\": a job still running has created nothing to \
                     report, got {job:?}"
                );
                assert!(
                    Instant::now() < give_up_at,
                    "case \"a settling job\": the job never reached its terminal status, \
                     last seen {job:?}"
                );
                tokio::time::sleep(SETTLEMENT_POLL_INTERVAL).await;
            }
            Some("SUCCESS") => break job,
            other => panic!(
                "case \"a settling job\": a job settles from PROCESSING into SUCCESS, \
                 got {other:?} in {job:?}"
            ),
        }
    };

    assert_eq!(
        settled["resources"],
        Value::Array(resources.iter().map(resource_body).collect()),
        "case \"a settling job\": a finished job reports what it created, got {settled:?}"
    );
    assert_eq!(
        settled["errorCode"],
        Value::Null,
        "case \"a settling job\": a job that succeeded reports no error, got {settled:?}"
    );
    assert_eq!(
        member_secs("a settling job", &settled, "createdAt"),
        epoch_secs(accepted_at),
        "case \"a settling job\": the instant the job was accepted must not move, \
         got {settled:?}"
    );
    assert_eq!(
        member_secs("a settling job", &settled, "updatedAt"),
        epoch_secs(due_at),
        "case \"a settling job\": a job is reported as updated when its terminal status \
         came due, got {settled:?}"
    );

    // And the terminal report is stable: a later read says the same thing.
    let again = em.list_jobs("").await;
    let listed = listed_jobs("a settled job, read again", &again);
    assert_eq!(
        listed, vec![settled],
        "case \"a settled job, read again\": a read must not change what a later caller \
         sees, got {listed:?}"
    );
}
