// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `GetCommandExecution` and `DeleteCommandExecution`: the
//! IoT control plane's reader and remover of the COMMAND executions that live in the
//! SHARED store (`service_common::iot_job_executions`) the IoT Jobs data plane's
//! `StartCommandExecution` writes.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 `GET`/`DELETE /command-executions/{executionId}?targetArn=…` signed for
//! the `iot` signing name — so what is asserted is what an AWS SDK client would see.
//!
//! The rules under test:
//!
//! * `GetCommandExecution` reports the STORED command execution the targeted device
//!   holds under that `executionId`, projected onto the model's response —
//!   `executionId`, `commandArn`, `targetArn`, `status` (the record's status on the
//!   `CommandExecutionStatus` surface, where a freshly enqueued execution is
//!   `CREATED`), `parameters` (the ones the start handed the device), `createdAt`,
//!   `lastUpdatedAt`, `startedAt` (only once a device took it up), `completedAt` (only
//!   once it finished), `executionTimeoutSeconds` and `timeToLive` — and reading it
//!   twice gives the same answer;
//! * a `targetArn` names a device by its ARN ALONE: a value that is no
//!   ARN at all is refused 400 `ValidationException` with AWS's
//!   FIXED sentence for that member — `Invalid targetArn`, naming the member and echoing
//!   no part of the request — while one that IS an ARN but can name no device of the caller's
//!   own account (another account's, another partition's, another resource type's) is
//!   refused 403 with the Coral-located access-denied error code and the
//!   capitalised `Message` member; the thing REGISTRY is no part of that reading, so an
//!   execution of a device nobody created is read and deleted like any other, and an
//!   `executionId` the device holds no command execution under is 404
//!   `ResourceNotFoundException`;
//! * an execution whose armed timeout the clock has already passed is reported
//!   `TIMED_OUT` — the transition that was due at the deadline, which the device's own
//!   plane then sees too — while a twin whose deadline is still ahead is untouched;
//! * `DeleteCommandExecution` removes a command execution a DEVICE reported the outcome
//!   of the work for (`SUCCEEDED`, `FAILED`, `REJECTED`) from the shared store, and
//!   refuses it in every other status it can be read in (`CREATED`, `IN_PROGRESS`,
//!   `TIMED_OUT`) 400 `ValidationException` with the service's fixed deletability
//!   message, changing nothing; an `executionId` the device holds nothing under removes
//!   nothing and answers 200, the model declaring no not-found error for the operation.
//!
//! Each table spans what its rule generalises over — which `CommandExecutionStatus` the
//! execution is in, how many parameters it carries and of which modelled type, whether
//! it carries a timeout and how far its deadline is, whether a device has started it,
//! the form of the device's name, the region the request is signed for, and the form of
//! the supplied `targetArn` — rather than one recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    command_execution_status, COMMAND_EXECUTION_RETENTION_SECONDS,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use service_common::iot_job_executions::{
    now_epoch_seconds, JobExecutionRecord, JobExecutionStore, STATUS_IN_PROGRESS, STATUS_QUEUED,
    STATUS_REJECTED, STATUS_TIMED_OUT,
};
use smithy_server_runtime::MultiServiceRouter;

// The one query-value percent-encoder this test crate has (`:` left literal, which RFC
// 3986 allows in a query); a `targetArn` travels through it exactly as an SDK sends it.
use crate::describe_endpoint::percent_encode;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for unless a case names another.
const REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name
/// and `region`: the scope is how the router picks the claiming service, and how the
/// service picks the region its ARNs are scoped to.
fn signed_for_iot(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{region}/iot/aws4_request, \
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
    /// A member of the response body as it was serialized; `Value::Null` when absent.
    fn member(&self, name: &str) -> &Value {
        self.body.get(name).unwrap_or(&Value::Null)
    }

    /// A timestamp member as the epoch seconds the restJson1 protocol renders it as,
    /// or `None` when the member is absent.
    fn instant(&self, name: &str) -> Option<f64> {
        self.body.get(name).and_then(Value::as_f64)
    }

    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// Epoch seconds as a reported timestamp compares.
fn instant_of(epoch_seconds: i64) -> Option<f64> {
    #[allow(clippy::cast_precision_loss)]
    Some(epoch_seconds as f64)
}

/// What one seeded command execution is: the record the IoT Jobs data plane's
/// `StartCommandExecution` leaves in the shared store, in whichever state a device has
/// since left it.
struct Seed<'a> {
    execution_id: &'a str,
    thing_name: &'a str,
    command_arn: String,
    /// The `parameters` object of the job document the start persisted.
    parameters: Value,
    /// `executionTimeoutSeconds` as the start was given it.
    timeout_seconds: Option<i64>,
    /// The deadline that timeout armed, in epoch seconds, when one is armed.
    timeout_expires_at: Option<i64>,
    /// The `JobExecutionStatus` the record holds — the device-facing vocabulary.
    status: &'a str,
    created_at: i64,
    started_at: Option<i64>,
    last_updated_at: i64,
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it. The
/// engine handle is kept so a case can seed the SHARED command-execution store the way
/// the IoT Jobs data plane writes it, and read back what the control plane left there.
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

    async fn call(&self, method: &str, path: &str, body: Option<&Value>, region: &str) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{region}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot(region))
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

    /// `CreateCommand`, so a seeded execution names a command that really exists.
    async fn create_command(&self, command_id: &str, region: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/commands/{command_id}"),
            Some(&json!({})),
            region,
        )
        .await
    }

    /// `CreateThing`, so the device a seeded execution belongs to is one this account's
    /// registry holds — which is what a case about a REGISTERED device needs, and what a
    /// case about an unregistered one must not have.
    async fn create_thing(&self, thing_name: &str, region: &str) -> Reply {
        self.call(
            "POST",
            &format!("/things/{thing_name}"),
            Some(&json!({})),
            region,
        )
        .await
    }

    /// `GetCommandExecution` for one execution of one target, signed for `region`.
    async fn get_execution(
        &self,
        execution_id: &str,
        target_arn: &str,
        include_result: Option<bool>,
        region: &str,
    ) -> Reply {
        let result_query = include_result
            .map(|value| format!("&includeResult={value}"))
            .unwrap_or_default();
        self.call(
            "GET",
            &format!(
                "/command-executions/{execution_id}?targetArn={}{result_query}",
                percent_encode(target_arn)
            ),
            None,
            region,
        )
        .await
    }

    /// `DeleteCommandExecution` for one execution of one target, signed for `region`.
    async fn delete_execution(&self, execution_id: &str, target_arn: &str, region: &str) -> Reply {
        self.call(
            "DELETE",
            &format!(
                "/command-executions/{execution_id}?targetArn={}",
                percent_encode(target_arn)
            ),
            None,
            region,
        )
        .await
    }

    /// Enqueue one COMMAND execution in the SHARED store the way
    /// `iot-jobs-data:StartCommandExecution` does — a job execution keyed by the minted
    /// `executionId`, carrying the command's ARN, the job document
    /// `{"commandArn": …, "parameters": …}` the device fetches and the execution
    /// timeout the start was given — and then put it in the state the row asks for. The
    /// DEVICE it belongs to is created first, through the real surface, so a case about a
    /// registered device names one the registry holds.
    ///
    /// Seeding the shared store directly is what lets ONE suite span every state a
    /// device can leave an execution in (a status only the device-facing plane reaches,
    /// an armed timer, a started execution); the cross-service suite
    /// (`core/router/tests/all/iot_command_execution_lifecycle.rs`) drives the real
    /// writer end to end.
    async fn seed(&self, seed: &Seed<'_>) {
        let created = self.create_thing(seed.thing_name, REGION).await;
        assert_eq!(
            created.status, 200,
            "the device {} must be creatable, got {created:?}",
            seed.thing_name
        );
        self.insert_execution(seed);
    }

    /// The same record as [`Emulator::seed`], for a device the thing REGISTRY does not
    /// hold — which is what `StartCommandExecution` leaves behind when it is fired at a
    /// target nobody created, the case the single-execution readers must answer from the
    /// execution store rather than from the registry.
    fn seed_without_the_device(&self, seed: &Seed<'_>) {
        self.insert_execution(seed);
    }

    /// The store write both seeders share.
    fn insert_execution(&self, seed: &Seed<'_>) {
        let document = json!({
            "commandArn": seed.command_arn,
            "parameters": seed.parameters,
        })
        .to_string();
        let mut record = JobExecutionRecord::queued(
            seed.execution_id.to_string(),
            seed.thing_name.to_string(),
            Some(document),
            seed.created_at,
        );
        record.command_arn = Some(seed.command_arn.clone());
        record.command_execution_timeout_seconds = seed.timeout_seconds;
        record.execution_timeout_expires_at = seed.timeout_expires_at;
        record.status = seed.status.to_string();
        record.started_at = seed.started_at;
        record.last_updated_at = seed.last_updated_at;

        let store = self.engine.state::<JobExecutionStore>();
        let mut store = store.lock();
        store.try_insert_execution(record).unwrap_or_else(|why| {
            panic!("seeding execution {}: {why}", seed.execution_id);
        });
    }

    /// Enqueue an execution a JOB created — no `commandArn` — so a case can prove it is
    /// not reachable as a command execution of the device, whose own record is created
    /// first for the same reason [`Emulator::seed`] creates one.
    async fn seed_job_execution(&self, job_id: &str, thing_name: &str, status: &str) {
        let created = self.create_thing(thing_name, REGION).await;
        assert_eq!(
            created.status, 200,
            "the device {thing_name} must be creatable, got {created:?}"
        );
        let mut record = JobExecutionRecord::queued(
            job_id.to_string(),
            thing_name.to_string(),
            Some(r#"{"operation":"reboot"}"#.to_string()),
            now_epoch_seconds(),
        );
        record.status = status.to_string();
        let store = self.engine.state::<JobExecutionStore>();
        let mut store = store.lock();
        store
            .try_insert_execution(record)
            .unwrap_or_else(|why| panic!("seeding job execution {job_id}: {why}"));
    }

    /// The thing names `ListThings` reports, which is what this account's REGISTRY holds:
    /// reading or deleting a command execution must put nothing in it.
    async fn registered_things(&self, region: &str) -> Vec<String> {
        let reply = self.call("GET", "/things", None, region).await;
        assert_eq!(reply.status, 200, "ListThings must answer 200, got {reply:?}");
        reply
            .member("things")
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|thing| thing.get("thingName").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    /// The status the SHARED store holds for (thing, execution) — what the device
    /// polling the Jobs data plane would see — or `None` when it holds no such record.
    fn stored_status(&self, thing_name: &str, execution_id: &str) -> Option<String> {
        let store = self.engine.state::<JobExecutionStore>();
        let store = store.lock();
        store
            .execution_index(thing_name, execution_id, None)
            .and_then(|index| store.record(thing_name, index))
            .map(|record| record.status.clone())
    }
}

/// The `CommandExecutionStatus` values this model declares
/// (`smithy_query(query="shape",shape="CommandExecutionStatus")`), spelled as the
/// response carries them. `CREATED` is the one with no `JobExecutionStatus` twin: the
/// shared record holds that state as `QUEUED`.
const STATUS_CREATED: &str = "CREATED";
const STATUS_SUCCEEDED: &str = "SUCCEEDED";
const STATUS_FAILED: &str = "FAILED";

/// A stored `JobExecutionStatus` the command surface has NO value for, so a record in
/// it is not a command execution any reader of this surface reports.
const STATUS_CANCELED: &str = "CANCELED";

/// The service's wording for a delete of a command execution that has not become
/// deletable: FIXED text for that refusal, carrying no value of the request or the
/// stored record.
const NOT_TERMINAL_MESSAGE: &str =
    "Failed to delete command execution as it hasn't become terminal.";

/// The ARN of a device, as a `targetArn` names it.
fn target_arn(region: &str, thing_name: &str) -> String {
    format!("arn:aws:iot:{region}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// The ARN `CreateCommand` mints for a command id in `region`.
fn command_arn(region: &str, command_id: &str) -> String {
    format!("arn:aws:iot:{region}:{ACCOUNT_ID}:command/{command_id}")
}

/// One row of the read-back table.
struct ReadCase {
    case: &'static str,
    execution_id: &'static str,
    thing_name: &'static str,
    region: &'static str,
    timeout_seconds: Option<i64>,
    /// The `JobExecutionStatus` the record is read in.
    stored_status: &'static str,
    /// Whether a device has taken the execution up.
    started: bool,
    /// What `status` the response must carry.
    expected_status: &'static str,
    /// Whether the response must carry `completedAt`.
    expect_completed_at: bool,
    /// `includeResult`, as the row asks for it.
    include_result: Option<bool>,
}

/// The rows differ along what the rule generalises over:
///
/// * the `CommandExecutionStatus` the execution is read in — every one of the six
///   `CommandExecutionStatus` values
///   (`smithy_query(query="shape",shape="CommandExecutionStatus")`: CREATED,
///   IN_PROGRESS, SUCCEEDED, FAILED, REJECTED, TIMED_OUT), reached from the
///   `JobExecutionStatus` the shared record holds;
/// * how many parameters the execution carries and of which modelled type —
///   `CommandParameterValue` carries one member per type (`S`, `B`, `I`, `L`, `D`,
///   `BIN`, `UL`), so one row carries every one of them and others carry one or none;
/// * whether an execution timeout was set and what it is —
///   `CommandExecutionTimeoutInSeconds` is modelled `range 1..`, so the rows include
///   the published minimum, a middling value and none at all;
/// * whether a device has started the execution, which is what `startedAt` reports;
/// * the form and length of the device's name (`ThingName`: len 1..128,
///   `^[a-zA-Z0-9:_-]+$`);
/// * the region the request is signed for, which the reported `targetArn` is scoped to;
/// * whether the caller asks for the device-reported result.
const READ_CASES: &[ReadCase] = &[
    ReadCase {
        case: "a freshly started execution with no parameters and no timeout",
        execution_id: "exec-created-minimal",
        thing_name: "laser-1",
        region: REGION,
        timeout_seconds: None,
        stored_status: STATUS_QUEUED,
        started: false,
        expected_status: STATUS_CREATED,
        expect_completed_at: false,
        include_result: None,
    },
    ReadCase {
        case: "a created execution with one string parameter and the minimum timeout",
        execution_id: "exec_created_1",
        thing_name: "laser:line-2_A",
        region: REGION,
        timeout_seconds: Some(1),
        stored_status: STATUS_QUEUED,
        started: false,
        expected_status: STATUS_CREATED,
        expect_completed_at: false,
        include_result: Some(false),
    },
    ReadCase {
        case: "an execution a device has taken up, with a parameter of every type",
        execution_id: "exec-in-progress",
        thing_name: "laser-3",
        region: REGION,
        timeout_seconds: Some(900),
        stored_status: STATUS_IN_PROGRESS,
        started: true,
        expected_status: STATUS_IN_PROGRESS,
        expect_completed_at: false,
        include_result: Some(true),
    },
    ReadCase {
        case: "an execution the device finished successfully",
        execution_id: "exec-succeeded",
        thing_name: "laser-4",
        region: REGION,
        timeout_seconds: Some(60),
        stored_status: STATUS_SUCCEEDED,
        started: true,
        expected_status: STATUS_SUCCEEDED,
        expect_completed_at: true,
        include_result: Some(true),
    },
    ReadCase {
        case: "an execution the device failed",
        execution_id: "exec-failed",
        thing_name: "laser-5",
        region: REGION,
        timeout_seconds: None,
        stored_status: STATUS_FAILED,
        started: true,
        expected_status: STATUS_FAILED,
        expect_completed_at: true,
        include_result: None,
    },
    ReadCase {
        case: "an execution the device rejected before starting it",
        execution_id: "exec-rejected",
        thing_name: "laser-6",
        region: REGION,
        timeout_seconds: Some(30),
        stored_status: STATUS_REJECTED,
        started: false,
        expected_status: STATUS_REJECTED,
        expect_completed_at: true,
        include_result: None,
    },
    ReadCase {
        case: "an execution whose timer already moved it, signed for another region",
        execution_id: "exec-timed-out",
        thing_name: "EU-laser_7",
        region: "eu-west-1",
        timeout_seconds: Some(5),
        stored_status: STATUS_TIMED_OUT,
        started: true,
        expected_status: STATUS_TIMED_OUT,
        expect_completed_at: true,
        include_result: Some(true),
    },
];

/// The parameters each read row carries, kept beside the table because a `const` table
/// cannot hold a built `serde_json::Value`. One row carries a parameter of EVERY
/// modelled `CommandParameterValue` member (`S`, `B`, `I`, `L`, `D`, `BIN`, `UL`), one
/// carries a single string parameter, and the rest carry none.
fn parameters_of(case: &ReadCase) -> Value {
    match case.execution_id {
        "exec_created_1" => json!({ "target": { "S": "on" } }),
        "exec-in-progress" => json!({
            "s": { "S": "on" },
            "b": { "B": true },
            "i": { "I": 42 },
            "l": { "L": 9_000_000_000_i64 },
            "d": { "D": 0.25 },
            // The bytes 0x00 0x01 0x02, base64 — a blob travels base64 on the wire.
            "bin": { "BIN": "AAEC" },
            "ul": { "UL": "18446744073709551615" },
        }),
        "exec-succeeded" => json!({ "reportTo": { "S": "s3://bucket/key" } }),
        _ => Value::Null,
    }
}

/// RULE: `GetCommandExecution` reports the stored command execution of the targeted
/// device, member for member, and reading it changes nothing a later caller sees.
#[tokio::test]
async fn a_stored_command_execution_is_read_back_from_the_shared_store() {
    let emulator = Emulator::new();
    let now = now_epoch_seconds();

    for case in READ_CASES {
        let label = case.case;
        let command_id = format!("cmd-{}", case.thing_name.replace(':', "-"));
        let created = emulator.create_command(&command_id, case.region).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreateCommand must succeed, got {created:?}"
        );
        let command = command_arn(case.region, &command_id);

        // One execution created a minute ago, last written half a minute ago.
        let created_at = now - 60;
        let last_updated_at = now - 30;
        let parameters = parameters_of(case);
        emulator.seed(&Seed {
            execution_id: case.execution_id,
            thing_name: case.thing_name,
            command_arn: command.clone(),
            parameters: parameters.clone(),
            timeout_seconds: case.timeout_seconds,
            // A deadline far ahead, so no row of THIS table is moved by the clock; the
            // timeout table below is where a due deadline is exercised.
            timeout_expires_at: case
                .timeout_seconds
                .map(|_| created_at + 24 * 60 * 60),
            status: case.stored_status,
            created_at,
            started_at: case.started.then_some(created_at + 5),
            last_updated_at,
        })
        .await;

        let target = target_arn(case.region, case.thing_name);
        let reply = emulator
            .get_execution(
                case.execution_id,
                &target,
                case.include_result,
                case.region,
            )
            .await;

        assert_eq!(
            reply.status, 200,
            "case {label:?}: expected 200, got {reply:?}"
        );
        assert_eq!(
            reply.member("executionId"),
            &json!(case.execution_id),
            "case {label:?}: expected the stored executionId, got {:?}",
            reply.member("executionId")
        );
        assert_eq!(
            reply.member("commandArn"),
            &json!(command),
            "case {label:?}: expected the command's ARN, got {:?}",
            reply.member("commandArn")
        );
        assert_eq!(
            reply.member("targetArn"),
            &json!(target),
            "case {label:?}: expected the device's ARN, got {:?}",
            reply.member("targetArn")
        );
        assert_eq!(
            reply.member("status"),
            &json!(case.expected_status),
            "case {label:?}: expected status {:?}, got {:?}",
            case.expected_status,
            reply.member("status")
        );
        assert_eq!(
            reply.instant("createdAt"),
            instant_of(created_at),
            "case {label:?}: expected createdAt {created_at}, got {:?}",
            reply.member("createdAt")
        );
        assert_eq!(
            reply.instant("lastUpdatedAt"),
            instant_of(last_updated_at),
            "case {label:?}: expected lastUpdatedAt {last_updated_at}, got {:?}",
            reply.member("lastUpdatedAt")
        );
        assert_eq!(
            reply.instant("startedAt"),
            instant_of(if case.started {
                created_at + 5
            } else {
                created_at
            }),
            "case {label:?}: startedAt is when the execution started on the device \
             — the device's own instant once it reported one, else the instant the \
             start created it, got {:?}",
            reply.member("startedAt")
        );
        assert_eq!(
            reply.instant("completedAt"),
            case.expect_completed_at
                .then(|| instant_of(last_updated_at))
                .flatten(),
            "case {label:?}: completedAt is reported only once the execution \
             finished, got {:?}",
            reply.member("completedAt")
        );
        assert_eq!(
            reply.member("executionTimeoutSeconds").as_i64(),
            case.timeout_seconds,
            "case {label:?}: expected the timeout the start was given, got {:?}",
            reply.member("executionTimeoutSeconds")
        );
        assert_eq!(
            reply.instant("timeToLive"),
            instant_of(created_at + COMMAND_EXECUTION_RETENTION_SECONDS),
            "case {label:?}: expected the documented retention from creation, got {:?}",
            reply.member("timeToLive")
        );
        assert_eq!(
            reply.member("parameters"),
            &parameters,
            "case {label:?}: expected the parameters the start handed the device, \
             got {:?}",
            reply.member("parameters")
        );
        // Nothing records a DEVICE-reported result or reason (the device-side
        // `UpdateCommandExecution` MQTT API is not served here), so neither member is
        // reported, whether or not the caller asked for the result.
        assert_eq!(
            (reply.member("result"), reply.member("statusReason")),
            (&Value::Null, &Value::Null),
            "case {label:?}: nothing persists a device-reported result or reason, \
             got {:?}",
            reply.body
        );

        // A read is a read: the second answer is the first, and the device-facing store
        // still holds the status it held.
        let again = emulator
            .get_execution(case.execution_id, &target, case.include_result, case.region)
            .await;
        assert_eq!(
            (again.status, &again.body),
            (reply.status, &reply.body),
            "case {label:?}: two reads of one execution must agree, got {again:?}"
        );
        assert_eq!(
            emulator.stored_status(case.thing_name, case.execution_id),
            Some(case.stored_status.to_string()),
            "case {label:?}: the shared store must be untouched by a read"
        );
    }
}

/// RULE: a `targetArn` that is no ARN at all is a malformed member — 400
/// `ValidationException` with AWS's fixed sentence for it — while one that IS an ARN but can
/// name no device of the caller's own account is the authorization refusal: 403 with the
/// Coral-located access-denied error code and the capitalised `Message` member. Neither
/// reads nor writes anything.
#[tokio::test]
async fn a_target_that_names_no_device_is_refused() {
    let emulator = Emulator::new();
    let thing_name = "laser-target-validation";
    emulator.create_command("cmd-validation", REGION).await;
    emulator.seed(&Seed {
        execution_id: "exec-validation",
        thing_name,
        command_arn: command_arn(REGION, "cmd-validation"),
        parameters: Value::Null,
        timeout_seconds: None,
        timeout_expires_at: None,
        status: STATUS_QUEUED,
        created_at: now_epoch_seconds(),
        started_at: None,
        last_updated_at: now_epoch_seconds(),
    })
    .await;

    // The rows span the ways a value can fail to name a device of this account: another
    // IoT resource type (including a thing GROUP, which is a set of devices and not one),
    // another service's ARN, another ACCOUNT's and another PARTITION's thing ARN, an ARN
    // with no resource name, a `thing/` ARN whose name no `/things/{thingName}` request
    // could address (`ThingName`: len 1..128, `^[a-zA-Z0-9:_-]+$`) — each of them an ARN,
    // so each the access-denied refusal — and the two values that are no ARN at all: a
    // bare name and the empty string (`TargetArn` is modelled len 0..2048, so it reaches
    // the handler). A thing ARN of this account the REGISTRY does not hold is not among
    // them: it names a device, and what that device holds is the execution store's answer
    // (`an_execution_of_an_unregistered_device_is_read_and_deleted_like_any_other`).
    let long_name = "x".repeat(129);
    // (case, targetArn, whether the value is an ARN at all)
    let cases: Vec<(&str, String, bool)> = vec![
        (
            "a thing group",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/fleet"),
            true,
        ),
        (
            "a job",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:job/upgrade-42"),
            true,
        ),
        (
            "a command",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:command/cmd-validation"),
            true,
        ),
        (
            "another service's thing ARN",
            format!("arn:aws:s3:{REGION}:{ACCOUNT_ID}:thing/{thing_name}"),
            true,
        ),
        (
            "another account's thing ARN",
            format!("arn:aws:iot:{REGION}:111122223333:thing/{thing_name}"),
            true,
        ),
        (
            "another partition's thing ARN",
            format!("arn:aws-cn:iot:cn-north-1:111122223333:thing/{thing_name}"),
            true,
        ),
        (
            "a thing ARN with no name",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/"),
            true,
        ),
        (
            "a thing name past the modelled maximum",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{long_name}"),
            true,
        ),
        (
            "a thing name outside the modelled character class",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/laser 8"),
            true,
        ),
        (
            "not an ARN at all",
            "laser-target-validation".to_string(),
            false,
        ),
        ("the empty target", String::new(), false),
    ];

    for (case, target, is_arn) in &cases {
        for (operation, reply) in [
            (
                "GetCommandExecution",
                emulator
                    .get_execution("exec-validation", target, None, REGION)
                    .await,
            ),
            (
                "DeleteCommandExecution",
                emulator
                    .delete_execution("exec-validation", target, REGION)
                    .await,
            ),
        ] {
            if *is_arn {
                // The ARN can name no device of the caller's account, which is an
                // authorization answer and not a constraint violation: the
                // error code carries the Coral locator and the text travels in a
                // capitalised `Message`, both spelled out here rather than taken from the
                // code under test.
                assert_eq!(
                    (reply.status, reply.error_code.as_deref()),
                    (
                        403,
                        Some(
                            "AccessDeniedException:http://internal.amazon.com/coral/\
                             com.amazon.coral.service/"
                        )
                    ),
                    "case {case:?}: {operation} must refuse an ARN outside the account as \
                     an authorization failure, got {reply:?}"
                );
                assert_eq!(
                    reply.member("Message").as_str(),
                    Some("targetArn not found in account"),
                    "case {case:?}: {operation}'s refusal must state AWS's fixed sentence \
                     under the capitalised member, got {reply:?}"
                );
            } else {
                assert_eq!(
                    (reply.status, reply.error_code.as_deref()),
                    (400, Some("ValidationException")),
                    "case {case:?}: {operation} must refuse a value that is no ARN as a \
                     malformed member, got {reply:?}"
                );
                // The sentence is AWS's, spelled out here rather than taken from the code
                // under test: fixed text naming the MEMBER, with no part of the request in
                // it.
                assert_eq!(
                    reply.message(),
                    "Invalid targetArn",
                    "case {case:?}: {operation}'s refusal must state AWS's fixed sentence \
                     for this member, got {reply:?}"
                );
            }
        }
    }

    // A refused request is a request that read and wrote nothing.
    assert_eq!(
        emulator.stored_status(thing_name, "exec-validation"),
        Some(STATUS_QUEUED.to_string()),
        "a refused request must leave the stored execution exactly as it was"
    );
}

/// RULE: the ownership answer comes FIRST. A request whose `targetArn` names no device of
/// this account is refused 403 whatever else about the request is malformed, because that
/// decision belongs to the layer in FRONT of the service — while the same malformed member
/// on a request whose target IS a device of the account is answered as the constraint
/// violation it is.
///
/// The rows span `CommandExecutionId` (len 1..64, `^[a-zA-Z0-9_-]+$`): a value the model
/// admits, one outside its character class, and one past its maximum.
#[tokio::test]
async fn a_target_outside_the_account_is_refused_before_the_requests_own_members() {
    let emulator = Emulator::new();
    let thing_name = "laser-ordering";
    emulator.create_command("cmd-ordering", REGION).await;
    emulator.seed(&Seed {
        execution_id: "exec-ordering",
        thing_name,
        command_arn: command_arn(REGION, "cmd-ordering"),
        parameters: Value::Null,
        timeout_seconds: None,
        timeout_expires_at: None,
        status: STATUS_QUEUED,
        created_at: now_epoch_seconds(),
        started_at: None,
        last_updated_at: now_epoch_seconds(),
    })
    .await;

    let held = target_arn(REGION, thing_name);
    // A target genuinely outside the account: another account's thing ARN. A thing ARN of
    // THIS account names a device whatever the registry holds, so it could not show this
    // ordering.
    let outside = format!("arn:aws:iot:{REGION}:111122223333:thing/laser-no-such-device");
    let too_long = "e".repeat(65);
    // (case, executionId, whether the model admits that id)
    let cases: [(&str, &str, bool); 3] = [
        ("an executionId the model admits", "exec-ordering", true),
        (
            "an executionId outside the modelled character class",
            "exec.ordering",
            false,
        ),
        (
            "an executionId past the modelled maximum",
            too_long.as_str(),
            false,
        ),
    ];

    for (case, execution_id, admitted) in cases {
        // The target outside the account: the authorization answer, whatever the id is.
        for (operation, reply) in [
            (
                "GetCommandExecution",
                emulator
                    .get_execution(execution_id, &outside, None, REGION)
                    .await,
            ),
            (
                "DeleteCommandExecution",
                emulator.delete_execution(execution_id, &outside, REGION).await,
            ),
        ] {
            assert_eq!(
                (reply.status, reply.member("Message").as_str()),
                (403, Some("targetArn not found in account")),
                "case {case:?}: {operation} must answer the ownership refusal before the \
                 request's own members, got {reply:?}"
            );
        }

        // The same id against a device this account holds: the member's own answer.
        let reply = emulator.get_execution(execution_id, &held, None, REGION).await;
        if admitted {
            assert_eq!(
                reply.status, 200,
                "case {case:?}: the admitted id names the seeded execution, got {reply:?}"
            );
        } else {
            // The shape is the one THIS operation models for an invalid request:
            // `GetCommandExecution`'s errors are `InternalServerException`,
            // `ResourceNotFoundException`, `ThrottlingException` and `ValidationException`
            // (`smithy_query(query="operation",operation="GetCommandExecution")`) — the
            // commands plane does not model the classic surface's legacy exception, so a
            // constraint refusal of it names `ValidationException`.
            assert_eq!(
                (reply.status, reply.error_code.as_deref()),
                (400, Some("ValidationException")),
                "case {case:?}: a malformed executionId on a device of this account is the \
                 constraint violation, got {reply:?}"
            );
        }
    }

    // Every refusal above left the seeded execution exactly as it was.
    assert_eq!(
        emulator.stored_status(thing_name, "exec-ordering"),
        Some(STATUS_QUEUED.to_string()),
        "a refused request must leave the stored execution exactly as it was"
    );
}

/// RULE: `GetCommandExecution` answers 404 `ResourceNotFoundException` for an
/// `executionId` the targeted device holds no COMMAND execution under — whichever half
/// of the pair is unknown, and including an execution a JOB created.
#[tokio::test]
async fn an_execution_the_device_does_not_hold_is_not_found() {
    let emulator = Emulator::new();
    emulator.create_command("cmd-missing", REGION).await;
    emulator.seed(&Seed {
        execution_id: "exec-present",
        thing_name: "laser-present",
        command_arn: command_arn(REGION, "cmd-missing"),
        parameters: Value::Null,
        timeout_seconds: None,
        timeout_expires_at: None,
        status: STATUS_QUEUED,
        created_at: now_epoch_seconds(),
        started_at: None,
        last_updated_at: now_epoch_seconds(),
    })
    .await;
    // An execution a JOB created on the same device: reachable as a job execution of
    // that job, never as a command execution of the device.
    emulator
        .seed_job_execution("job-not-a-command", "laser-present", STATUS_QUEUED)
        .await;
    // A COMMAND execution whose stored status this model has no value for: no
    // `CommandExecutionStatus` names `CANCELED`, so no reader of this surface reports
    // the record rather than inventing a status the model does not declare.
    emulator.seed(&Seed {
        execution_id: "exec-canceled",
        thing_name: "laser-present",
        command_arn: command_arn(REGION, "cmd-missing"),
        parameters: Value::Null,
        timeout_seconds: None,
        timeout_expires_at: None,
        status: STATUS_CANCELED,
        created_at: now_epoch_seconds(),
        started_at: None,
        last_updated_at: now_epoch_seconds(),
    })
    .await;
    // Two more DEVICES of this account, holding no command execution of their own: the
    // rows below are about an execution the device does not hold, so each of them names a
    // device the registry does hold.
    for thing_name in ["laser-empty", "laser-other"] {
        let created = emulator.create_thing(thing_name, REGION).await;
        assert_eq!(
            created.status, 200,
            "the device {thing_name} must be creatable, got {created:?}"
        );
    }

    let cases: [(&str, &str, &str); 5] = [
        (
            "an execution id nothing is stored under",
            "exec-absent",
            "laser-present",
        ),
        (
            "a device that holds no execution at all",
            "exec-present",
            "laser-empty",
        ),
        (
            "an execution of another device",
            "exec-present",
            "laser-other",
        ),
        (
            "a job execution, not a command execution",
            "job-not-a-command",
            "laser-present",
        ),
        (
            "a status no CommandExecutionStatus names",
            "exec-canceled",
            "laser-present",
        ),
    ];

    for (case, execution_id, thing_name) in cases {
        let reply = emulator
            .get_execution(execution_id, &target_arn(REGION, thing_name), None, REGION)
            .await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case:?}: expected 404 ResourceNotFoundException, got {reply:?}"
        );
    }

    // The job execution the delete could not see is still the device's job execution.
    assert_eq!(
        emulator.stored_status("laser-present", "job-not-a-command"),
        Some(STATUS_QUEUED.to_string()),
        "a job execution must survive every command-execution request"
    );
}

/// RULE: the `TIMED_OUT` transition an armed timeout has already made due is
/// materialised before the execution is reported, so the control plane and the device's
/// own plane see one status; an execution whose deadline is still ahead is untouched.
#[tokio::test]
async fn a_due_timeout_is_materialised_before_the_execution_is_reported() {
    let emulator = Emulator::new();
    emulator.create_command("cmd-timeout", REGION).await;
    let command = command_arn(REGION, "cmd-timeout");
    let now = now_epoch_seconds();

    // The rows put one deadline on each side of the instant the read happens, and one
    // exactly at it: the move is due AT the deadline.
    let cases: [(&str, i64, &str); 4] = [
        ("a deadline long past", -3_600, STATUS_TIMED_OUT),
        ("a deadline one second past", -1, STATUS_TIMED_OUT),
        ("a deadline due this second", 0, STATUS_TIMED_OUT),
        ("a deadline still an hour ahead", 3_600, STATUS_CREATED),
    ];

    for (index, (case, offset, expected_status)) in cases.into_iter().enumerate() {
        let thing_name = format!("laser-timeout-{index}");
        let execution_id = format!("exec-timeout-{index}");
        emulator.seed(&Seed {
            execution_id: &execution_id,
            thing_name: &thing_name,
            command_arn: command.clone(),
            parameters: Value::Null,
            timeout_seconds: Some(600),
            timeout_expires_at: Some(now + offset),
            status: STATUS_QUEUED,
            created_at: now - 600,
            started_at: None,
            last_updated_at: now - 600,
        })
        .await;

        let target = target_arn(REGION, &thing_name);
        let reply = emulator
            .get_execution(&execution_id, &target, None, REGION)
            .await;
        assert_eq!(
            (reply.status, reply.member("status")),
            (200, &json!(expected_status)),
            "case {case:?}: expected status {expected_status:?}, got {reply:?}"
        );
        // Whatever the reader reported is what the device's plane now holds: the move
        // was owed at the deadline, so it is recorded and not merely rendered.
        let stored = emulator.stored_status(&thing_name, &execution_id);
        assert_eq!(
            stored.as_deref().and_then(command_execution_status),
            Some(expected_status),
            "case {case:?}: the shared store must hold the status the deadline \
             decided, got {stored:?}"
        );
        // A timed-out execution has finished, so it reports when: the deadline it was
        // stamped at, never the instant it happened to be read at.
        if expected_status == STATUS_TIMED_OUT {
            assert_eq!(
                reply.instant("completedAt"),
                instant_of(now + offset),
                "case {case:?}: a timed-out execution completed at its deadline, \
                 got {:?}",
                reply.member("completedAt")
            );
        }
    }
}

/// One row of the deletability table.
struct DeleteCase {
    case: &'static str,
    /// The `JobExecutionStatus` the record is stored in.
    stored_status: &'static str,
    /// Where the armed execution timeout's deadline falls relative to the instant the
    /// delete is made, in seconds; `None` arms no timer at all. A deadline already past
    /// is the `TIMED_OUT` move the clock owes, which the delete materialises before it
    /// reads the status.
    timeout_offset: Option<i64>,
    /// Whether the delete removes the execution.
    deletable: bool,
    /// The `CommandExecutionStatus` the execution is reported in after the call, when it
    /// survives it.
    reported_status: &'static str,
}

/// RULE: `DeleteCommandExecution` removes a command execution only in a status a DEVICE
/// reported the outcome of the work with — `SUCCEEDED`, `FAILED` or `REJECTED` — and
/// refuses it in every other status it can be read in (`CREATED`, `IN_PROGRESS`, and
/// `TIMED_OUT`, whether the record was stored that way or the clock moved it there) with
/// HTTP 400 `ValidationException` carrying the service's fixed deletability message,
/// leaving the execution exactly as it was and still readable.
#[tokio::test]
async fn only_a_device_reported_command_execution_is_deleted() {
    let emulator = Emulator::new();
    emulator.create_command("cmd-delete", REGION).await;
    let command = command_arn(REGION, "cmd-delete");
    let now = now_epoch_seconds();

    // One row per `CommandExecutionStatus` this model declares
    // (`smithy_query(query="shape",shape="CommandExecutionStatus")`: CREATED,
    // IN_PROGRESS, SUCCEEDED, FAILED, REJECTED, TIMED_OUT), plus the TIMED_OUT the CLOCK
    // makes due during the delete itself, and rows that differ in whether a timer is
    // armed at all.
    let cases: [DeleteCase; 8] = [
        DeleteCase {
            case: "a created execution",
            stored_status: STATUS_QUEUED,
            timeout_offset: Some(3_600),
            deletable: false,
            reported_status: STATUS_CREATED,
        },
        DeleteCase {
            case: "an execution in progress",
            stored_status: STATUS_IN_PROGRESS,
            timeout_offset: Some(3_600),
            deletable: false,
            reported_status: STATUS_IN_PROGRESS,
        },
        DeleteCase {
            case: "an execution with no timer, in progress",
            stored_status: STATUS_IN_PROGRESS,
            timeout_offset: None,
            deletable: false,
            reported_status: STATUS_IN_PROGRESS,
        },
        DeleteCase {
            case: "an execution the clock timed out at a deadline already past",
            stored_status: STATUS_QUEUED,
            timeout_offset: Some(-1),
            deletable: false,
            reported_status: STATUS_TIMED_OUT,
        },
        DeleteCase {
            case: "an execution already stored timed out",
            stored_status: STATUS_TIMED_OUT,
            timeout_offset: None,
            deletable: false,
            reported_status: STATUS_TIMED_OUT,
        },
        DeleteCase {
            case: "a succeeded execution",
            stored_status: STATUS_SUCCEEDED,
            timeout_offset: Some(3_600),
            deletable: true,
            reported_status: STATUS_SUCCEEDED,
        },
        DeleteCase {
            case: "a failed execution, no timer",
            stored_status: STATUS_FAILED,
            timeout_offset: None,
            deletable: true,
            reported_status: STATUS_FAILED,
        },
        DeleteCase {
            case: "a rejected execution",
            stored_status: STATUS_REJECTED,
            timeout_offset: Some(3_600),
            deletable: true,
            reported_status: STATUS_REJECTED,
        },
    ];

    for (index, row) in cases.iter().enumerate() {
        let case = row.case;
        let thing_name = format!("laser-delete-{index}");
        let execution_id = format!("exec-delete-{index}");
        emulator.seed(&Seed {
            execution_id: &execution_id,
            thing_name: &thing_name,
            command_arn: command.clone(),
            parameters: json!({ "target": { "S": "off" } }),
            timeout_seconds: row.timeout_offset.map(|_| 120),
            timeout_expires_at: row.timeout_offset.map(|offset| now + offset),
            status: row.stored_status,
            created_at: now - 120,
            started_at: Some(now - 100),
            last_updated_at: now - 60,
        })
        .await;

        let target = target_arn(REGION, &thing_name);
        let reply = emulator
            .delete_execution(&execution_id, &target, REGION)
            .await;

        if row.deletable {
            assert_eq!(
                reply.status, 200,
                "case {case:?}: an execution the device reported an outcome for is \
                 deletable, got {reply:?}"
            );
            assert_eq!(
                emulator.stored_status(&thing_name, &execution_id),
                None,
                "case {case:?}: a deleted execution must be gone from the shared store"
            );
            let gone = emulator
                .get_execution(&execution_id, &target, None, REGION)
                .await;
            assert_eq!(
                (gone.status, gone.error_code.as_deref()),
                (404, Some("ResourceNotFoundException")),
                "case {case:?}: a deleted execution must no longer be readable, \
                 got {gone:?}"
            );
            // Deleting it again removes nothing and is not an error the model declares.
            let repeat = emulator
                .delete_execution(&execution_id, &target, REGION)
                .await;
            assert_eq!(
                repeat.status, 200,
                "case {case:?}: a repeated delete removes nothing and answers 200, \
                 got {repeat:?}"
            );
        } else {
            assert_eq!(
                (reply.status, reply.error_code.as_deref()),
                (400, Some("ValidationException")),
                "case {case:?}: an execution no device reported an outcome for is \
                 refused 400 ValidationException, got {reply:?}"
            );
            assert_eq!(
                reply.message(),
                NOT_TERMINAL_MESSAGE,
                "case {case:?}: expected the fixed deletability message, got {reply:?}"
            );
            // The refusal's text is FIXED: it carries neither the requested id nor the
            // status the store holds.
            assert!(
                !reply.message().contains(&execution_id)
                    && !reply.message().contains(row.reported_status),
                "case {case:?}: the refusal must interpolate no request or stored value, \
                 got {reply:?}"
            );
            let stored = emulator.stored_status(&thing_name, &execution_id);
            assert_eq!(
                stored.as_deref().and_then(command_execution_status),
                Some(row.reported_status),
                "case {case:?}: a refused delete must leave the execution in the status \
                 the clock decided, got {stored:?}"
            );
            let still_there = emulator
                .get_execution(&execution_id, &target, None, REGION)
                .await;
            assert_eq!(
                (still_there.status, still_there.member("status")),
                (200, &json!(row.reported_status)),
                "case {case:?}: a refused delete leaves the execution readable, \
                 got {still_there:?}"
            );
        }
    }
}

/// RULE: a delete of an `executionId` the device holds no command execution under
/// removes nothing and answers 200 — `DeleteCommandExecution` models no not-found error
/// (`smithy_query(query="operation",operation="DeleteCommandExecution")`) — and an
/// execution a JOB created is not reachable through it at all.
#[tokio::test]
async fn a_delete_of_what_the_device_does_not_hold_removes_nothing() {
    let emulator = Emulator::new();
    emulator
        .seed_job_execution("job-protected", "laser-untouched", STATUS_QUEUED)
        .await;
    emulator.create_command("cmd-untouched", REGION).await;
    // A command execution in a status this model cannot name is not one this surface
    // holds, so the delete must not remove it either.
    emulator.seed(&Seed {
        execution_id: "exec-canceled",
        thing_name: "laser-untouched",
        command_arn: command_arn(REGION, "cmd-untouched"),
        parameters: Value::Null,
        timeout_seconds: None,
        timeout_expires_at: None,
        status: STATUS_CANCELED,
        created_at: now_epoch_seconds(),
        started_at: None,
        last_updated_at: now_epoch_seconds(),
    })
    .await;
    // A DEVICE of this account holding nothing at all, so the row about a device that
    // holds nothing is about the execution store and not about the registry.
    let created = emulator.create_thing("laser-unknown", REGION).await;
    assert_eq!(
        created.status, 200,
        "the device laser-unknown must be creatable, got {created:?}"
    );

    let cases: [(&str, &str, &str); 4] = [
        (
            "an execution id nothing is stored under",
            "exec-never-existed",
            "laser-untouched",
        ),
        (
            "a device that holds nothing at all",
            "exec-never-existed",
            "laser-unknown",
        ),
        (
            "a job execution, not a command execution",
            "job-protected",
            "laser-untouched",
        ),
        (
            "a status no CommandExecutionStatus names",
            "exec-canceled",
            "laser-untouched",
        ),
    ];

    for (case, execution_id, thing_name) in cases {
        let reply = emulator
            .delete_execution(execution_id, &target_arn(REGION, thing_name), REGION)
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: a delete of what the device does not hold answers 200, \
             got {reply:?}"
        );
    }

    assert_eq!(
        emulator.stored_status("laser-untouched", "job-protected"),
        Some(STATUS_QUEUED.to_string()),
        "a job execution must never be removed by a command-execution delete"
    );
    assert_eq!(
        emulator.stored_status("laser-untouched", "exec-canceled"),
        Some(STATUS_CANCELED.to_string()),
        "a record this surface reports nothing for must not be removed by it either"
    );
}

/// One row of the unregistered-device table.
struct GhostCase {
    case: &'static str,
    /// The device the execution belongs to, which NO `CreateThing` ever created.
    thing_name: &'static str,
    execution_id: &'static str,
    region: &'static str,
    /// The `JobExecutionStatus` the record is stored in.
    stored_status: &'static str,
    /// Where the armed timeout's deadline falls relative to the read, in seconds;
    /// `None` arms no timer.
    timeout_offset: Option<i64>,
    /// The `CommandExecutionStatus` the read must report.
    reported_status: &'static str,
    /// Whether the delete removes the execution (only a status a DEVICE reported an
    /// outcome with is deletable).
    deletable: bool,
}

/// RULE: which device a command-execution `targetArn` names is read off the ARN alone, so
/// the thing REGISTRY decides nothing about it: an execution stored for a device the
/// registry does not hold — which is what `StartCommandExecution` leaves when it is fired
/// at a target nobody created — is reported, timed out and deleted exactly like one of a
/// registered device, an `executionId` such a device holds nothing under is the modelled
/// 404, and reading it registers no thing.
///
/// The rows span what the rule generalises over: the `CommandExecutionStatus` the record
/// is read in (including the `TIMED_OUT` the clock owes, which is the only status an
/// execution nothing can pick up ever reaches by itself), whether a timer is armed and on
/// which side of the read its deadline falls, the form of the device's name (`ThingName`:
/// len 1..128, `^[a-zA-Z0-9:_-]+$`) and the region the request is signed for.
const GHOST_CASES: &[GhostCase] = &[
    GhostCase {
        case: "a created execution whose deadline is still ahead",
        thing_name: "ghost-laser-1",
        execution_id: "exec-ghost-created",
        region: REGION,
        stored_status: STATUS_QUEUED,
        timeout_offset: Some(3_600),
        reported_status: STATUS_CREATED,
        deletable: false,
    },
    GhostCase {
        case: "an execution whose armed timeout the clock has passed",
        thing_name: "ghost:laser_2",
        execution_id: "exec-ghost-timed-out",
        region: REGION,
        stored_status: STATUS_QUEUED,
        timeout_offset: Some(-1),
        reported_status: STATUS_TIMED_OUT,
        deletable: false,
    },
    GhostCase {
        case: "an execution in progress with no timer at all",
        thing_name: "ghost-laser-3",
        execution_id: "exec_ghost_in_progress",
        region: REGION,
        stored_status: STATUS_IN_PROGRESS,
        timeout_offset: None,
        reported_status: STATUS_IN_PROGRESS,
        deletable: false,
    },
    GhostCase {
        case: "an execution the device reported succeeded",
        thing_name: "ghost-laser-4",
        execution_id: "exec-ghost-succeeded",
        region: REGION,
        stored_status: STATUS_SUCCEEDED,
        timeout_offset: Some(600),
        reported_status: STATUS_SUCCEEDED,
        deletable: true,
    },
    GhostCase {
        case: "a rejected execution, signed for another region",
        thing_name: "GHOST-laser_5",
        execution_id: "exec-ghost-rejected",
        region: "eu-west-1",
        stored_status: STATUS_REJECTED,
        timeout_offset: None,
        reported_status: STATUS_REJECTED,
        deletable: true,
    },
];

#[tokio::test]
async fn an_execution_of_an_unregistered_device_is_read_and_deleted_like_any_other() {
    let emulator = Emulator::new();
    let now = now_epoch_seconds();

    for case in GHOST_CASES {
        let label = case.case;
        let command_id = format!("cmd-{}", case.execution_id);
        let created = emulator.create_command(&command_id, case.region).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the command must be creatable, got {created:?}"
        );
        // The record the start leaves for a target the registry does not hold: no
        // `CreateThing` is made for this device, here or anywhere in the case.
        emulator.seed_without_the_device(&Seed {
            execution_id: case.execution_id,
            thing_name: case.thing_name,
            command_arn: command_arn(case.region, &command_id),
            parameters: json!({ "mode": { "S": "reboot" } }),
            timeout_seconds: case.timeout_offset.map(|_| 120),
            timeout_expires_at: case.timeout_offset.map(|offset| now + offset),
            status: case.stored_status,
            created_at: now - 120,
            started_at: None,
            last_updated_at: now - 120,
        });

        let target = target_arn(case.region, case.thing_name);
        let reply = emulator
            .get_execution(case.execution_id, &target, None, case.region)
            .await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (200, None),
            "case {label:?}: the execution of a device the registry does not hold must be \
             readable, got {reply:?}"
        );
        assert_eq!(
            (
                reply.member("executionId"),
                reply.member("targetArn"),
                reply.member("status"),
            ),
            (
                &json!(case.execution_id),
                &json!(target),
                &json!(case.reported_status),
            ),
            "case {label:?}: expected the stored execution of {target} in status {:?}, \
             got {reply:?}",
            case.reported_status
        );
        // The status the reader reported is the one the shared store now holds: the
        // `TIMED_OUT` move was owed at the deadline, and nothing else moved.
        let stored = emulator.stored_status(case.thing_name, case.execution_id);
        assert_eq!(
            stored.as_deref().and_then(command_execution_status),
            Some(case.reported_status),
            "case {label:?}: expected the shared store to hold {:?}, got {stored:?}",
            case.reported_status
        );
        // Reading an execution of a device nobody created does not create that device.
        let registered = emulator.registered_things(case.region).await;
        assert!(
            !registered.iter().any(|thing| thing == case.thing_name),
            "case {label:?}: reading the execution must register no thing, got {registered:?}"
        );

        // The device holds exactly the execution the start left: another id is the
        // modelled not-found answer, not an authorization refusal.
        let absent = emulator
            .get_execution("exec-ghost-absent", &target, None, case.region)
            .await;
        assert_eq!(
            (absent.status, absent.error_code.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {label:?}: an id this device holds nothing under is 404, got {absent:?}"
        );

        let deleted = emulator
            .delete_execution(case.execution_id, &target, case.region)
            .await;
        if case.deletable {
            assert_eq!(
                deleted.status, 200,
                "case {label:?}: an execution the device reported an outcome for is \
                 deletable, got {deleted:?}"
            );
            assert_eq!(
                emulator.stored_status(case.thing_name, case.execution_id),
                None,
                "case {label:?}: a deleted execution must be gone from the shared store"
            );
        } else {
            assert_eq!(
                (deleted.status, deleted.error_code.as_deref()),
                (400, Some("ValidationException")),
                "case {label:?}: an execution no device reported an outcome for is refused \
                 400 ValidationException, got {deleted:?}"
            );
            assert_eq!(
                deleted.message(),
                NOT_TERMINAL_MESSAGE,
                "case {label:?}: expected the fixed deletability message, got {deleted:?}"
            );
            assert_eq!(
                emulator
                    .stored_status(case.thing_name, case.execution_id)
                    .as_deref()
                    .and_then(command_execution_status),
                Some(case.reported_status),
                "case {label:?}: a refused delete must leave the execution exactly as it was"
            );
        }
    }
}
