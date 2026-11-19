// SPDX-License-Identifier: CC0-1.0

//! The over-the-air (OTA) TASK round trip, end to end over the wire: `CreateOtaTask` ->
//! `GetOtaTask` -> `ListOtaTasks`.
//!
//! Every case drives the real restJson1 request path (`MultiServiceRouter::dispatch`), so
//! the routes, status codes, bodies and modelled errors asserted here are the ones a client
//! sees. The suite carries its own request harness, as each suite in this crate does, so a
//! case reads as one HTTP call.
//!
//! RULES under test, each stated once and then checked over a TABLE of inputs rather than
//! over one recorded request:
//!
//! * a create persists the task the request described under a freshly minted `OtaTaskId`
//!   scoped to the request's region, and answers with that id and its ARN;
//! * both readers project the STORED record: every member the create carried comes back
//!   unchanged, every member it omitted stays absent, and the reported `OtaStatus` is
//!   DERIVED from the schedule the create carried and the clock;
//! * a list reports the tasks of ITS region in creation order, bounded by `MaxResults`,
//!   with a `NextToken` that resumes at the record after the last one reported;
//! * a create that NAMES another resource of this service — a managed thing on `Target`,
//!   an OTA task configuration on `TaskConfigurationId` — resolves that reference, so a
//!   name nothing of this region holds is refused and persists nothing;
//! * a replayed `ClientToken` (`@idempotencyToken`) answers with what the first call
//!   created instead of creating a second record.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::state::{
    MANAGED_THING_RESOURCE_TYPE, MAX_OTA_TASK_TARGETS, OTA_STATUS_IN_PROGRESS,
    OTA_STATUS_SCHEDULED, OTA_TASK_PAGE_LIMIT,
};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::{
    ComAmazonawsIotmanagedintegrationsIotmanagedintegrationsState as ServiceState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{audit_capture, label, AUDIT_TARGET};

/// The region the test's requests are signed for; the ARNs the service builds carry it.
const REGION: &str = "us-east-1";

/// The account id every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The route `CreateOtaTask` and `ListOtaTasks` are bound to
/// (`smithy_query(query="operation",operation="CreateOtaTask,ListOtaTasks")` ->
/// `POST /ota-tasks`, `GET /ota-tasks`).
const COLLECTION: &str = "/ota-tasks";

/// `OtaDescription`'s published `@length` maximum, from
/// `smithy_query(query="shape",shape="OtaDescription")` (len 1..256, pattern
/// `^[0-9A-Za-z_\- ]+$`).
const DESCRIPTION_MAX_LENGTH: usize = 256;

/// `S3Url`'s published `@length` maximum, from
/// `smithy_query(query="shape",shape="S3Url")` (len 1..1000).
const S3_URL_MAX_LENGTH: usize = 1_000;

/// The members of the create request that `GetOtaTask` projects back. `TaskId`, `TaskArn`,
/// `CreatedAt`, `LastUpdatedAt` and `Status` are the service's own and are asserted
/// separately.
const ECHOED_MEMBERS: &[&str] = &[
    "Description",
    "S3Url",
    "Protocol",
    "OtaType",
    "OtaTargetQueryString",
    "OtaMechanism",
    "Target",
    "TaskConfigurationId",
    "OtaSchedulingConfig",
    "OtaTaskExecutionRetryConfig",
    "Tags",
];

/// How the service words a `Target` or `TaskConfigurationId` naming a managed thing this
/// account holds no record of — the reference path's reason, which names the kind of record
/// and quotes nothing of the request.
const MANAGED_THING_REFERENCE_ABSENT: &str = "ManagedThing record does not exist";

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

    fn seconds(&self, name: &str) -> Option<f64> {
        self.member(name).and_then(Value::as_f64)
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
    emulator_over(Arc::new(engine::InMemoryEngine::new()))
}

/// [`emulator`] over an engine the caller keeps a handle to, so a test can inspect the
/// service's store directly.
fn emulator_over(engine: Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // The signature itself is not under test here, so requests are unauthenticated.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Send one request and reduce the answer. The `Authorization` credential scope is what
/// routes the request to this service and names the region its records belong to.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path_and_query: &str,
    body: Option<Value>,
) -> Reply {
    call_signed(router, REGION, method, path_and_query, body).await
}

/// [`call`], signed for an explicit region, so a regional resource can be addressed through
/// another region's endpoint.
async fn call_signed(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path_and_query: &str,
    body: Option<Value>,
) -> Reply {
    call_as(router, DEFAULT_AKID, region, method, path_and_query, body).await
}

/// The access key id every call of this suite presents unless it is checking attribution.
const DEFAULT_AKID: &str = "AKIDTESTTESTTESTTEST";

/// [`call_signed`], presenting an explicit access key id, so a test can tell the records
/// ITS calls left from every other suite's.
async fn call_as(
    router: &MultiServiceRouter,
    akid: &str,
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
                "AWS4-HMAC-SHA256 Credential={akid}/20260101/{region}/\
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

/// The minimal create request: the two members the model marks `@required`
/// (`smithy_query(query="operation",operation="CreateOtaTask")` -> `S3Url`, `OtaType`).
fn minimal_create(ota_type: &str) -> Value {
    json!({"S3Url": "s3://fleet-updates/firmware-1.0.bin", "OtaType": ota_type})
}

/// `CreateOtaTask` on `request`, asserting the modelled 201, the shape of the generated id
/// and the ARN built from it, and returning that id.
async fn create(router: &MultiServiceRouter, case: &str, request: Value) -> String {
    let reply = call(router, "POST", COLLECTION, Some(request.clone())).await;
    assert_eq!(
        reply.status, 201,
        "case {case:?}: expected HTTP 201 from CreateOtaTask, got {} with body {:?}",
        reply.status, reply.body
    );
    let id = reply
        .string("TaskId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a TaskId, got body {:?}", reply.body))
        .to_string();
    // `OtaTaskId`: len 1..200, pattern ^[A-Za-z0-9]+$.
    assert!(
        !id.is_empty() && id.len() <= 200 && id.chars().all(|c| c.is_ascii_alphanumeric()),
        "case {case:?}: TaskId must match ^[A-Za-z0-9]+$ (len 1..200), got {id:?}"
    );
    let expected_arn = format!("arn:aws:iotmanagedintegrations:{REGION}:{ACCOUNT}:ota-task/{id}");
    assert_eq!(
        reply.string("TaskArn"),
        Some(expected_arn.as_str()),
        "case {case:?}: TaskArn must be the ota-task ARN of the generated id, got {:?}",
        reply.member("TaskArn")
    );
    assert_eq!(
        reply.member("Description"),
        request.get("Description"),
        "case {case:?}: the create must answer with the description it stored, got {:?}",
        reply.member("Description")
    );
    id
}

/// `CreateManagedThing` for a device an OTA task can target, returning `(id, arn)`.
async fn managed_thing(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let reply = call(
        router,
        "POST",
        "/managed-things",
        Some(json!({
            "Role": "DEVICE",
            "AuthenticationMaterialType": "WIFI_SETUP_QR_BAR_CODE",
            "AuthenticationMaterial": format!("material for {name}"),
            "Name": name,
        })),
    )
    .await;
    assert_eq!(
        reply.status, 201,
        "seeding the managed thing {name:?}: expected HTTP 201, got {} with body {:?}",
        reply.status, reply.body
    );
    let id = reply
        .string("Id")
        .unwrap_or_else(|| panic!("seeding {name:?}: expected an Id, got {:?}", reply.body))
        .to_string();
    let arn = reply
        .string("Arn")
        .unwrap_or_else(|| panic!("seeding {name:?}: expected an Arn, got {:?}", reply.body))
        .to_string();
    (id, arn)
}

/// `CreateOtaTaskConfiguration`, returning the id a task may roll out under.
async fn task_configuration(router: &MultiServiceRouter, name: &str) -> String {
    let reply = call(
        router,
        "POST",
        "/ota-task-configurations",
        Some(json!({"Name": name})),
    )
    .await;
    assert_eq!(
        reply.status, 201,
        "seeding the task configuration {name:?}: expected HTTP 201, got {} with body {:?}",
        reply.status, reply.body
    );
    reply
        .string("TaskConfigurationId")
        .unwrap_or_else(|| {
            panic!(
                "seeding {name:?}: expected a TaskConfigurationId, got {:?}",
                reply.body
            )
        })
        .to_string()
}

/// A `ScheduleStartTime` far enough in the future that a task carrying it is still waiting
/// for its window however slowly the host runs, and one far enough in the past that the
/// window has opened. Both are RFC 3339 timestamps in the `Z` form.
const FUTURE_START: &str = "2999-01-01T00:00:00Z";
const PAST_START: &str = "2000-01-01T00:00:00Z";

/// The same two instants in the NUMERIC-OFFSET form RFC 3339 also admits — what a client
/// building its timestamp with `isoformat()` sends — so the status derivation is checked
/// over both zone forms rather than only the one the recorded request used.
const FUTURE_START_WITH_OFFSET: &str = "2999-01-01T00:00:00+02:00";
const PAST_START_WITH_OFFSET: &str = "2000-01-01T00:00:00-05:30";

/// RULE (see the module docs): a read projects the stored record, so every member a create
/// carried comes back unchanged, every member it omitted stays absent, and the reported
/// status follows the schedule the create carried.
#[tokio::test]
async fn a_read_projects_what_the_create_stored() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));

    let (thing_id, thing_arn) = managed_thing(&router, "lamp one").await;
    let (second_thing_id, _) = managed_thing(&router, "lamp two").await;
    let configuration = task_configuration(&router, "fleet rollout").await;

    // The rows differ along what the rule generalises over: which optional members are
    // present, which enum value each carries, how many targets the task has and in which
    // form they are named, how deeply the scheduling and retry structures nest, values at
    // the published bounds of each constrained member, and whether the schedule has
    // started. The recorded request this issue's unit carries is one row ("a one-time task
    // targeting a device by its ARN").
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "only the required members",
            minimal_create("ONE_TIME"),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "the other OtaType value",
            minimal_create("CONTINUOUS"),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "a description and both enum members",
            json!({
                "S3Url": "s3://fleet-updates/firmware-1.1.bin",
                "OtaType": "ONE_TIME",
                "Description": "lamp fleet firmware 1 1",
                "Protocol": "HTTP",
                "OtaMechanism": "PUSH",
            }),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "a description and an S3 URL at their published length bounds",
            json!({
                "S3Url": "s".repeat(S3_URL_MAX_LENGTH),
                "OtaType": "ONE_TIME",
                "Description": "d".repeat(DESCRIPTION_MAX_LENGTH),
            }),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "a one-character description and S3 URL (the length minimum)",
            json!({"S3Url": "s", "OtaType": "CONTINUOUS", "Description": "d"}),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "a target query string instead of a target list",
            json!({
                "S3Url": "s3://fleet-updates/firmware-1.1.bin",
                "OtaType": "CONTINUOUS",
                "OtaTargetQueryString": "attributes.brand:acme",
            }),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "a one-time task targeting a device by its ARN",
            json!({
                "S3Url": "s3://fleet-updates/firmware-1.2.bin",
                "OtaType": "ONE_TIME",
                "Target": [thing_arn.clone()],
                "TaskConfigurationId": configuration.clone(),
            }),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "two targets, one named by id and one by ARN, reported in the order sent",
            json!({
                "S3Url": "s3://fleet-updates/firmware-1.2.bin",
                "OtaType": "ONE_TIME",
                "Target": [second_thing_id.clone(), thing_arn.clone()],
            }),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "a target named by its managed-thing id",
            json!({
                "S3Url": "s3://fleet-updates/firmware-1.2.bin",
                "OtaType": "ONE_TIME",
                "Target": [thing_id.clone()],
            }),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "a schedule whose start time has not arrived",
            json!({
                "S3Url": "s3://fleet-updates/firmware-1.3.bin",
                "OtaType": "CONTINUOUS",
                "OtaSchedulingConfig": {"StartTime": FUTURE_START},
            }),
            OTA_STATUS_SCHEDULED,
        ),
        (
            "a start time that has not arrived, in the numeric-offset zone form",
            json!({
                "S3Url": "s3://fleet-updates/firmware-1.3.bin",
                "OtaType": "CONTINUOUS",
                "OtaSchedulingConfig": {"StartTime": FUTURE_START_WITH_OFFSET},
            }),
            OTA_STATUS_SCHEDULED,
        ),
        (
            "a start time that has passed, in the numeric-offset zone form",
            json!({
                "S3Url": "s3://fleet-updates/firmware-1.3.bin",
                "OtaType": "CONTINUOUS",
                "OtaSchedulingConfig": {"StartTime": PAST_START_WITH_OFFSET},
            }),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "a whole schedule, whose start time has passed",
            json!({
                "S3Url": "s3://fleet-updates/firmware-1.3.bin",
                "OtaType": "CONTINUOUS",
                "OtaSchedulingConfig": {
                    "StartTime": PAST_START,
                    "EndTime": FUTURE_START,
                    "EndBehavior": "FORCE_CANCEL",
                    "MaintenanceWindows": [
                        {"StartTime": PAST_START, "DurationInMinutes": 1},
                        {"StartTime": FUTURE_START, "DurationInMinutes": 1_430},
                    ],
                },
            }),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "a schedule whose start time is not a date-time this service reads",
            json!({
                "S3Url": "s3://fleet-updates/firmware-1.3.bin",
                "OtaType": "CONTINUOUS",
                "OtaSchedulingConfig": {"StartTime": "cron(0 12 * * ? *)"},
            }),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "a retry config at the published bounds of its criteria",
            json!({
                "S3Url": "s3://fleet-updates/firmware-1.4.bin",
                "OtaType": "ONE_TIME",
                "OtaTaskExecutionRetryConfig": {
                    "RetryConfigCriteria": [
                        {"FailureType": "FAILED", "MinNumberOfRetries": 0},
                        {"FailureType": "TIMED_OUT", "MinNumberOfRetries": 10},
                        {"FailureType": "ALL", "MinNumberOfRetries": 5},
                    ],
                },
            }),
            OTA_STATUS_IN_PROGRESS,
        ),
        (
            "every member at once",
            json!({
                "S3Url": "s3://fleet-updates/firmware-2.0.bin",
                "OtaType": "CONTINUOUS",
                "Description": "whole request",
                "Protocol": "HTTP",
                "OtaMechanism": "PUSH",
                "OtaTargetQueryString": "attributes.model:lamp",
                "Target": [thing_id.clone()],
                "TaskConfigurationId": configuration.clone(),
                "OtaSchedulingConfig": {"StartTime": FUTURE_START, "EndBehavior": "CANCEL"},
                "OtaTaskExecutionRetryConfig": {
                    "RetryConfigCriteria": [{"FailureType": "ALL", "MinNumberOfRetries": 1}],
                },
                "Tags": {"team": "fleet", "stage": ""},
            }),
            OTA_STATUS_SCHEDULED,
        ),
    ];

    let mut ids = BTreeSet::new();
    for (case, request, expected_status) in &cases {
        let id = create(&router, case, request.clone()).await;
        assert!(
            ids.insert(id.clone()),
            "case {case:?}: each create must mint a fresh id, got {id:?} twice"
        );

        let read = call(&router, "GET", &format!("{COLLECTION}/{id}"), None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected HTTP 200 from GetOtaTask, got {} with body {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.string("TaskId"),
            Some(id.as_str()),
            "case {case:?}: the read must report the created id, got {:?}",
            read.member("TaskId")
        );
        for stamp in ["CreatedAt", "LastUpdatedAt"] {
            assert!(
                read.seconds(stamp).is_some_and(|secs| secs > 0.0),
                "case {case:?}: expected a {stamp} timestamp, got {:?}",
                read.member(stamp)
            );
        }
        assert_eq!(
            read.string("Status"),
            Some(*expected_status),
            "case {case:?}: the status must follow the schedule the create carried \
             (expected {expected_status:?}), got {:?}",
            read.member("Status")
        );
        // Nothing in this emulator runs a task against a device, so the task reports no
        // processing details rather than counts for executions that never happened.
        assert_eq!(
            read.member("TaskProcessingDetails"),
            None,
            "case {case:?}: expected no TaskProcessingDetails, got {:?}",
            read.member("TaskProcessingDetails")
        );
        for member in ECHOED_MEMBERS {
            let sent = request.get(member);
            assert_eq!(
                read.member(member),
                sent,
                "case {case:?}: the read must report {member:?} exactly as the create carried \
                 it (expected {sent:?}), got {:?}",
                read.member(member)
            );
        }
    }

    assert_eq!(
        engine.state::<ServiceState>().lock().ota_tasks.len(),
        cases.len(),
        "every create must have persisted exactly one record"
    );
}

/// RULE: a list reports the tasks of ITS region in creation order, bounded by `MaxResults`,
/// with a `NextToken` that resumes at the record after the last one reported; each summary
/// projects the same record the task's own read projects.
#[tokio::test]
async fn a_list_pages_over_the_regions_tasks_in_creation_order() {
    let router = emulator();

    let empty = call(&router, "GET", COLLECTION, None).await;
    assert_eq!(
        empty.status, 200,
        "case \"empty store\": expected HTTP 200 from ListOtaTasks, got {} with body {:?}",
        empty.status, empty.body
    );
    assert!(
        empty.tasks().is_empty() && empty.member("NextToken").is_none(),
        "case \"empty store\": expected no tasks and no NextToken, got body {:?}",
        empty.body
    );

    let configuration = task_configuration(&router, "paging rollout").await;
    let mut created = Vec::new();
    for index in 0..5u32 {
        let mut request = minimal_create("ONE_TIME");
        request["Description"] = json!(format!("rollout {index}"));
        if index % 2 == 0 {
            request["TaskConfigurationId"] = json!(configuration.clone());
        }
        created.push(create(&router, "paging", request).await);
    }

    // The rows differ along what the rule generalises over: the page size, from one record
    // per page to more than the store holds, plus the absent `MaxResults` that gets the
    // service's own page limit.
    let page_sizes = [Some(1u32), Some(2), Some(4), Some(5), Some(6), None];
    for page_size in page_sizes {
        let case = format!("MaxResults={page_size:?}");
        let mut seen = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut query = Vec::new();
            if let Some(max) = page_size {
                query.push(format!("MaxResults={max}"));
            }
            if let Some(next) = &token {
                query.push(format!("NextToken={next}"));
            }
            let path = if query.is_empty() {
                COLLECTION.to_string()
            } else {
                format!("{COLLECTION}?{}", query.join("&"))
            };
            let page = call(&router, "GET", &path, None).await;
            assert_eq!(
                page.status, 200,
                "case {case:?}: expected HTTP 200 from ListOtaTasks, got {} with body {:?}",
                page.status, page.body
            );
            let items = page.tasks();
            let expected_len = page_size
                .map_or(created.len(), |max| (max as usize).min(OTA_TASK_PAGE_LIMIT))
                .min(created.len() - seen.len());
            assert_eq!(
                items.len(),
                expected_len,
                "case {case:?}: a page must carry at most MaxResults items, got {items:?}"
            );
            for item in &items {
                let task_id = item
                    .get("TaskId")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("case {case:?}: a summary needs a TaskId: {item:?}"));
                // The summary and the task's own read project ONE record, so the members
                // they share cannot disagree.
                let read = call(&router, "GET", &format!("{COLLECTION}/{task_id}"), None).await;
                for member in ["TaskArn", "CreatedAt", "LastUpdatedAt", "Status"] {
                    assert_eq!(
                        item.get(member).filter(|value| !value.is_null()),
                        read.member(member),
                        "case {case:?}: the summary's {member} must match the task's own read, \
                         got {:?} against {:?}",
                        item.get(member),
                        read.member(member)
                    );
                }
                assert_eq!(
                    item.get("TaskConfigurationId")
                        .filter(|value| !value.is_null()),
                    read.member("TaskConfigurationId"),
                    "case {case:?}: the summary's TaskConfigurationId must match the read, got \
                     {:?} against {:?}",
                    item.get("TaskConfigurationId"),
                    read.member("TaskConfigurationId")
                );
                seen.push(task_id.to_string());
            }
            pages += 1;
            assert!(
                pages <= created.len() + 1,
                "case {case:?}: paging must terminate, got {pages} pages for {} records",
                created.len()
            );
            match page.string("NextToken") {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            seen, created,
            "case {case:?}: paging must report every record once, in creation order"
        );
    }

    // A task belongs to the region that created it, so another region's endpoint lists
    // none of them.
    for other_region in ["us-west-2", "eu-central-1"] {
        let elsewhere = call_signed(&router, other_region, "GET", COLLECTION, None).await;
        assert!(
            elsewhere.tasks().is_empty(),
            "case {other_region:?}: a list must report only its own region's tasks, got {:?}",
            elsewhere.body
        );
    }

    // A list is a read: asking twice reports the same page, and nothing it reported has
    // changed.
    let first = call(&router, "GET", COLLECTION, None).await;
    let second = call(&router, "GET", COLLECTION, None).await;
    assert_eq!(
        first.body, second.body,
        "case \"list twice\": a list must not change what a later caller sees, got {:?} then {:?}",
        first.body, second.body
    );
}

/// RULE: a `NextToken` this service did not issue is the modelled `ValidationException`
/// rather than an empty page.
#[tokio::test]
async fn a_next_token_this_service_did_not_issue_is_refused() {
    let router = emulator();
    create(&router, "seed", minimal_create("ONE_TIME")).await;

    // The rows differ in HOW the token fails to be one of this service's cursors: the
    // wrong length, characters outside a cursor, and a well-formed cursor beyond every
    // sequence the store has handed out.
    let cases = [
        ("too short", "00"),
        ("not hexadecimal", "zzzzzzzzzzzzzzzz"),
        ("one past the minted sequence", "00000000000000ff"),
        ("the largest cursor", "ffffffffffffffff"),
    ];
    for (case, token) in cases {
        let reply = call(
            &router,
            "GET",
            &format!("{COLLECTION}?NextToken={token}"),
            None,
        )
        .await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected HTTP 400 for a token this service cannot have issued, got \
             {} with body {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some("ValidationException"),
            "case {case:?}: expected the modelled ValidationException code, got {:?}",
            reply.error_type
        );
    }
}

/// RULE: a create that names another resource of this service resolves that reference, so a
/// name nothing of this region holds is refused with the modelled
/// `ResourceNotFoundException` and persists nothing.
#[tokio::test]
async fn a_create_naming_an_absent_reference_is_refused_and_stores_nothing() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));

    let (thing_id, thing_arn) = managed_thing(&router, "stored lamp").await;
    let configuration = task_configuration(&router, "stored rollout").await;
    let absent_id = "a".repeat(32);
    let absent_arn =
        format!("arn:aws:iotmanagedintegrations:{REGION}:{ACCOUNT}:managed-thing/{absent_id}");
    let other_region_arn =
        format!("arn:aws:iotmanagedintegrations:us-west-2:{ACCOUNT}:managed-thing/{thing_id}");

    // The rows differ along what the rule generalises over: which reference is absent, and
    // in what form it was named.
    let managed_thing_cases: Vec<(&str, Value, &str)> = vec![
        (
            "an id nothing was ever created under",
            json!([absent_id.clone()]),
            absent_id.as_str(),
        ),
        (
            "an ARN of this region whose id nothing holds",
            json!([absent_arn.clone()]),
            absent_arn.as_str(),
        ),
        (
            "the ARN of a stored thing, but of another region",
            json!([other_region_arn.clone()]),
            other_region_arn.as_str(),
        ),
        (
            "one absent entry beside a stored one",
            json!([thing_id.clone(), absent_id.clone()]),
            absent_id.as_str(),
        ),
        ("an empty target entry", json!([""]), ""),
    ];

    for (case, target, reported) in &managed_thing_cases {
        let mut request = minimal_create("ONE_TIME");
        request["Target"] = target.clone();
        let reply = call(&router, "POST", COLLECTION, Some(request)).await;
        assert_eq!(
            reply.status, 404,
            "case {case:?}: expected HTTP 404 for a target naming no managed thing, got {} with \
             body {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected the modelled ResourceNotFoundException code, got {:?}",
            reply.error_type
        );
        assert_eq!(
            reply.string("Message"),
            Some(MANAGED_THING_REFERENCE_ABSENT),
            "case {case:?}: the reason must state that the named record does not exist, got {:?}",
            reply.member("Message")
        );
        assert_eq!(
            reply.string("ResourceId"),
            Some(*reported),
            "case {case:?}: the 404 must report the reference as the request spelled it, got {:?}",
            reply.member("ResourceId")
        );
        assert_eq!(
            reply.string("ResourceType"),
            Some(MANAGED_THING_RESOURCE_TYPE),
            "case {case:?}: the 404 must report the kind of record that is missing, got {:?}",
            reply.member("ResourceType")
        );
    }

    // The same rule for the OTA task configuration a task rolls out under: this resource's
    // own path words its absence, quoting the identifier the request carried.
    let mut near_miss = configuration.clone();
    near_miss.pop();
    near_miss.push(if configuration.ends_with('z') { 'y' } else { 'z' });
    for (case, configuration_id) in [
        ("an id nothing was ever created under", "b".repeat(32)),
        ("one character off a stored id", near_miss),
    ] {
        let mut request = minimal_create("CONTINUOUS");
        request["TaskConfigurationId"] = json!(configuration_id.clone());
        let reply = call(&router, "POST", COLLECTION, Some(request)).await;
        assert_eq!(
            reply.status, 404,
            "case {case:?}: expected HTTP 404 for an absent task configuration, got {} with body \
             {:?}",
            reply.status, reply.body
        );
        let expected = format!("OTA task configuration not found for id: {configuration_id}");
        assert_eq!(
            reply.string("Message"),
            Some(expected.as_str()),
            "case {case:?}: the reason must quote the identifier the request carried, got {:?}",
            reply.member("Message")
        );
    }

    // A target list past the bound one task may hold is refused before any reference is
    // resolved; the bound itself is admitted, and reaches the reference check.
    let at_the_bound: Vec<String> = (0..MAX_OTA_TASK_TARGETS)
        .map(|index| format!("absent{index}"))
        .collect();
    let mut request = minimal_create("ONE_TIME");
    request["Target"] = json!(at_the_bound.clone());
    let admitted = call(&router, "POST", COLLECTION, Some(request)).await;
    assert_eq!(
        admitted.status, 404,
        "case \"targets at the bound\": the bound itself must be admitted and resolved, got {} \
         with body {:?}",
        admitted.status, admitted.body
    );

    let mut one_past = at_the_bound;
    one_past.push("absent-one-past".to_string());
    let mut request = minimal_create("ONE_TIME");
    request["Target"] = json!(one_past);
    let refused = call(&router, "POST", COLLECTION, Some(request)).await;
    assert_eq!(
        refused.status, 400,
        "case \"targets one past the bound\": expected HTTP 400, got {} with body {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_type.as_deref(),
        Some("ValidationException"),
        "case \"targets one past the bound\": expected the modelled ValidationException code, got \
         {:?}",
        refused.error_type
    );

    assert!(
        engine.state::<ServiceState>().lock().ota_tasks.is_empty(),
        "a refused create must not have persisted a record"
    );
    let list = call(&router, "GET", COLLECTION, None).await;
    assert!(
        list.tasks().is_empty(),
        "a refused create must leave the list empty, got {:?}",
        list.body
    );

    // The stored thing and configuration are still usable, so the refusals above rejected
    // the reference rather than the request's shape.
    let accepted = json!({
        "S3Url": "s3://fleet-updates/firmware-1.0.bin",
        "OtaType": "ONE_TIME",
        "Target": [thing_arn],
        "TaskConfigurationId": configuration,
    });
    create(&router, "stored references", accepted).await;
    assert_eq!(
        engine.state::<ServiceState>().lock().ota_tasks.len(),
        1,
        "an accepted create must have persisted exactly one record"
    );
}

/// RULE: an `Identifier` the store does not hold is answered with the modelled
/// `ResourceNotFoundException` — HTTP 404, whose reason quotes the identifier the request
/// carried — and the read persists nothing.
#[tokio::test]
async fn a_read_of_an_unknown_identifier_is_the_modelled_not_found() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));

    let stored = create(&router, "seed", minimal_create("ONE_TIME")).await;

    // Identifiers the model admits (^[A-Za-z0-9]+$, len 1..200) that were never minted:
    // the shortest one, one at the published length maximum, and one differing from a
    // stored id in its last character.
    let mut near_miss = stored.clone();
    near_miss.pop();
    near_miss.push(if stored.ends_with('z') { 'y' } else { 'z' });
    let cases = [
        ("one character", "a".to_string()),
        ("two hundred characters", "b".repeat(200)),
        ("one character off a stored id", near_miss),
    ];

    for (case, identifier) in &cases {
        let reply = call(&router, "GET", &format!("{COLLECTION}/{identifier}"), None).await;
        assert_eq!(
            reply.status, 404,
            "case {case:?}: expected HTTP 404 for an identifier that was never minted, got {} \
             with body {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected the modelled ResourceNotFoundException code, got {:?}",
            reply.error_type
        );
        let expected = format!("OTA task not found for id: {identifier}");
        assert_eq!(
            reply.string("Message"),
            Some(expected.as_str()),
            "case {case:?}: the reason must quote the identifier the request carried, got {:?}",
            reply.member("Message")
        );
        // The reason is the whole body: this resource's read 404 fills neither optional
        // member of the error shape.
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

    assert_eq!(
        engine.state::<ServiceState>().lock().ota_tasks.len(),
        1,
        "a refused read must not have written anything"
    );
}

/// RULE: a task is a regional resource, so only the region that created it resolves it; and
/// a read changes nothing a later caller sees.
#[tokio::test]
async fn a_task_resolves_only_through_the_region_that_created_it() {
    let router = emulator();

    let id = create(&router, "regional", minimal_create("ONE_TIME")).await;

    for other_region in ["us-west-2", "eu-central-1"] {
        let elsewhere = call_signed(
            &router,
            other_region,
            "GET",
            &format!("{COLLECTION}/{id}"),
            None,
        )
        .await;
        assert_eq!(
            elsewhere.status, 404,
            "case {other_region:?}: a record created in {REGION:?} must not resolve through \
             another region, got {} with body {:?}",
            elsewhere.status, elsewhere.body
        );
    }

    let first = call(&router, "GET", &format!("{COLLECTION}/{id}"), None).await;
    let second = call(&router, "GET", &format!("{COLLECTION}/{id}"), None).await;
    assert_eq!(
        first.status, 200,
        "case \"own region\": expected HTTP 200, got {} with body {:?}",
        first.status, first.body
    );
    assert_eq!(
        first.body, second.body,
        "case \"read twice\": a read must not change what a later caller sees, got {:?} then {:?}",
        first.body, second.body
    );
}

/// RULE: a retry carrying a `ClientToken` an earlier successful create used answers with
/// what that call created and performs no further action; a create under a token nobody used
/// creates its own record.
#[tokio::test]
async fn a_replayed_client_token_answers_with_the_first_record() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));

    // `ClientToken`: len 1..64, pattern ^[a-zA-Z0-9=_-]+$. The rows differ in the token's
    // form and in which members the create carries.
    let cases: Vec<(&str, String, Value)> = vec![
        ("a bare token", "token1".to_string(), minimal_create("ONE_TIME")),
        (
            "a token using every admitted character class",
            "aZ9=_-".to_string(),
            json!({
                "S3Url": "s3://fleet-updates/firmware-1.1.bin",
                "OtaType": "CONTINUOUS",
                "Description": "retry safe",
                "OtaSchedulingConfig": {"StartTime": FUTURE_START},
            }),
        ),
        (
            "a token at the published length maximum",
            "b".repeat(64),
            minimal_create("CONTINUOUS"),
        ),
    ];

    let mut minted = BTreeSet::new();
    for (case, token, mut request) in cases {
        request["ClientToken"] = json!(token);
        let first = call(&router, "POST", COLLECTION, Some(request.clone())).await;
        let replayed = call(&router, "POST", COLLECTION, Some(request.clone())).await;
        assert_eq!(
            first.status, 201,
            "case {case:?}: expected HTTP 201 from the first create, got {} with body {:?}",
            first.status, first.body
        );
        assert_eq!(
            replayed.status, 201,
            "case {case:?}: expected HTTP 201 from the replay, got {} with body {:?}",
            replayed.status, replayed.body
        );
        assert_eq!(
            replayed.body, first.body,
            "case {case:?}: a replayed ClientToken must answer with the first record, got {:?} \
             against {:?}",
            replayed.body, first.body
        );
        let id = first
            .string("TaskId")
            .unwrap_or_else(|| panic!("case {case:?}: expected a TaskId, got {:?}", first.body))
            .to_string();
        assert!(
            minted.insert(id.clone()),
            "case {case:?}: a token nobody used must mint its own record, got {id:?} twice"
        );
    }

    assert_eq!(
        engine.state::<ServiceState>().lock().ota_tasks.len(),
        minted.len(),
        "each replayed create must have performed no further action"
    );
}

/// RULE: the ARN-addressed tagging surface reads and writes the SAME tag map the resource's
/// own read reports, for every resource kind this service stores — an OTA task among them,
/// now that one is stored (`IoTManagedIntegrationsResourceARN` admits `ota-task`).
#[tokio::test]
async fn the_tagging_surface_reaches_a_task_by_its_arn() {
    let router = emulator();

    let mut request = minimal_create("ONE_TIME");
    request["Tags"] = json!({"team": "fleet"});
    let id = create(&router, "tagged", request).await;
    let arn = format!("arn:aws:iotmanagedintegrations:{REGION}:{ACCOUNT}:ota-task/{id}");

    let listed = call(&router, "GET", &format!("/tags/{}", label(&arn)), None).await;
    assert_eq!(
        listed.status, 200,
        "case \"created tags\": expected HTTP 200 from ListTagsForResource, got {} with body {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.member("tags"),
        Some(&json!({"team": "fleet"})),
        "case \"created tags\": the tagging surface must report what the create stored, got {:?}",
        listed.member("tags")
    );

    let tagged = call(
        &router,
        "POST",
        &format!("/tags/{}", label(&arn)),
        Some(json!({"Tags": {"stage": "beta"}})),
    )
    .await;
    assert_eq!(
        tagged.status, 200,
        "case \"add a tag\": expected HTTP 200 from TagResource, got {} with body {:?}",
        tagged.status, tagged.body
    );
    let read = call(&router, "GET", &format!("{COLLECTION}/{id}"), None).await;
    assert_eq!(
        read.member("Tags"),
        Some(&json!({"team": "fleet", "stage": "beta"})),
        "case \"add a tag\": the task's own read must report the pair the tagging call wrote, \
         got {:?}",
        read.member("Tags")
    );

    let untagged = call(
        &router,
        "DELETE",
        &format!("/tags/{}?tagKeys=team", label(&arn)),
        None,
    )
    .await;
    assert_eq!(
        untagged.status, 200,
        "case \"remove a tag\": expected HTTP 200 from UntagResource, got {} with body {:?}",
        untagged.status, untagged.body
    );
    let read = call(&router, "GET", &format!("{COLLECTION}/{id}"), None).await;
    assert_eq!(
        read.member("Tags"),
        Some(&json!({"stage": "beta"})),
        "case \"remove a tag\": the task's own read must report the removal, got {:?}",
        read.member("Tags")
    );
}

/// The access key id this suite's audited calls present, so the records they leave can be
/// told from every sibling suite's running in parallel over the same routes.
const AUDIT_AKID: &str = "AKIDOTATASKAUDIT0001";

/// RULE: a call that CHANGES state leaves one structured, attributable record on the
/// `emulator::audit` target, naming the operation, the identity the request presented, the
/// region it was signed for, the resource it ended up with and how the call ended — so a
/// create of an over-the-air task is attributable locally.
#[tokio::test]
async fn a_state_changing_create_leaves_one_attributable_audit_record() {
    let captured = audit_capture();
    let router = emulator();

    // A record the capture answers for before the calls under test, so a stale record
    // cannot be mistaken for one of theirs.
    captured.drain_for(AUDIT_AKID);

    let mut request = minimal_create("ONE_TIME");
    request["ClientToken"] = json!("auditedtoken1");
    let created = call_as(
        &router,
        AUDIT_AKID,
        REGION,
        "POST",
        COLLECTION,
        Some(request.clone()),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case \"created\": expected HTTP 201 from CreateOtaTask, got {} with body {:?}",
        created.status, created.body
    );
    let id = created
        .string("TaskId")
        .unwrap_or_else(|| panic!("case \"created\": expected a TaskId, got {:?}", created.body))
        .to_string();

    // The rows differ along what the rule generalises over: the OUTCOME the record names —
    // the create that wrote the record, and the retry that replayed it and wrote none of
    // its own.
    let replayed = call_as(
        &router,
        AUDIT_AKID,
        REGION,
        "POST",
        COLLECTION,
        Some(request),
    )
    .await;
    assert_eq!(
        replayed.status, 201,
        "case \"replayed\": expected HTTP 201 from the retry, got {} with body {:?}",
        replayed.status, replayed.body
    );

    let records = captured.drain_for(AUDIT_AKID);
    assert_eq!(
        records.len(),
        2,
        "expected one record per state-changing call, got {records:?}"
    );
    for (case, outcome, record) in [
        ("created", "created", &records[0]),
        ("replayed", "replayed", &records[1]),
    ] {
        for field in [
            format!("{AUDIT_TARGET} "),
            "operation=CreateOtaTask".to_string(),
            format!("akid={AUDIT_AKID}"),
            format!("region={REGION}"),
            format!("task_id={id}"),
            format!("outcome={outcome}"),
        ] {
            assert!(
                record.contains(&field),
                "case {case:?}: the audit record must carry {field:?}, got {record:?}"
            );
        }
    }
}
