// SPDX-License-Identifier: CC0-1.0

//! THE DEVICE DEFENDER AUDIT-TASK PLANE's wire contract, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rules under test, none of them specific to one audit, one check or one id:
//!
//! * an account that has set up NO audit configuration is refused the ACCOUNT-LEVEL
//!   precondition by every operation that targets audit checks, before any check the
//!   request named is looked at — a different refusal, and a different remediation, from
//!   the per-check one an account that HAS a configuration gets for a check it has not
//!   enabled;
//! * a `taskId` the account started no audit under is reported missing by one sentence,
//!   whichever operation addressed the task and whatever form the id took;
//! * the id `StartOnDemandAuditTask` mints is a hyphen-free lowercase hexadecimal token of
//!   fixed width, unique per audit, and the audit is readable back under it;
//! * every check entry `DescribeAuditTask` reports carries the three resource counters,
//!   zero while the check has performed none of them — one still in progress, and one the
//!   cancel of its audit stopped — and the counts of what it evaluated once it has run.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    ComAmazonawsIotAwsiotserviceState, AUDIT_SPAN, AUDIT_TASK_COMPLETED, AUDIT_TASK_IN_PROGRESS,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite is signed for. An audit is account-wide, so no
/// assertion depends on it.
const REGION: &str = "eu-central-1";

/// The role the account's audit configuration names, of more than the modelled minimum
/// length (`RoleArn`, len 20..2048).
const AUDIT_ROLE_ARN: &str = "arn:aws:iam::123456789012:role/device-defender-audit";

/// The refusal an operation that targets audit checks answers an account that has set up
/// no audit configuration with — fixed text, with no part of the request in it.
const ACCOUNT_NOT_SET_UP: &str =
    "Account Audit Configuration for your account have not been set up yet";

/// Audit checks the rows below name individually. Each is an entry of the emulator's
/// catalog of Device Defender audit checks
/// (`service_com_amazonaws_iot_awsiotservice::state::AUDIT_CHECK_NAMES`).
const LOGGING_DISABLED_CHECK: &str = "LOGGING_DISABLED_CHECK";
const DEVICE_CERTIFICATE_AGE_CHECK: &str = "DEVICE_CERTIFICATE_AGE_CHECK";
const DEVICE_CERTIFICATE_EXPIRING_CHECK: &str = "DEVICE_CERTIFICATE_EXPIRING_CHECK";
const IOT_POLICY_OVERLY_PERMISSIVE_CHECK: &str = "IOT_POLICY_OVERLY_PERMISSIVE_CHECK";

/// The three resource counters the model declares on `AuditCheckDetails`
/// (`smithy_query(query="shape",shape="AuditCheckDetails")`).
const RESOURCE_COUNTERS: &[&str] = &[
    "totalResourcesCount",
    "nonCompliantResourcesCount",
    "suppressedNonCompliantResourcesCount",
];

/// One emulator: the shared engine plus a router with this service mounted.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the audit request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    /// Set the account's audit configuration up, enabling `check_names`.
    async fn enable_checks(&self, case: &str, check_names: &[&str]) {
        let mut checks = Map::new();
        for check_name in check_names {
            checks.insert((*check_name).to_string(), json!({ "enabled": true }));
        }
        let reply = self
            .dispatch(
                "PATCH",
                "/audit/configuration",
                Some(&json!({
                    "roleArn": AUDIT_ROLE_ARN,
                    "auditCheckConfigurations": Value::Object(checks),
                })),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected UpdateAccountAuditConfiguration to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    async fn start_audit(&self, targets: &[&str]) -> Reply {
        self.dispatch(
            "POST",
            "/audit/tasks",
            Some(&json!({ "targetCheckNames": targets })),
        )
        .await
    }

    /// Start an audit over `targets` and require it to succeed, answering its task id.
    async fn started_audit(&self, case: &str, targets: &[&str]) -> String {
        let reply = self.start_audit(targets).await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected StartOnDemandAuditTask to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["taskId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no taskId in {}", reply.text()))
            .to_string()
    }

    /// Create the scheduled audit `name` over `targets`, whatever the answer.
    async fn create_scheduled_audit(&self, name: &str, targets: &[&str]) -> Reply {
        self.dispatch(
            "POST",
            &format!("/audit/scheduledaudits/{name}"),
            Some(&json!({
                "frequency": "DAILY",
                "targetCheckNames": targets,
            })),
        )
        .await
    }

    async fn describe_audit(&self, task_id: &str) -> Reply {
        self.dispatch("GET", &format!("/audit/tasks/{task_id}"), None)
            .await
    }

    async fn cancel_audit(&self, task_id: &str) -> Reply {
        self.dispatch("PUT", &format!("/audit/tasks/{task_id}/cancel"), None)
            .await
    }

    /// The audit `task_id`, read back and required to succeed.
    async fn reported_audit(&self, case: &str, task_id: &str) -> Value {
        let reply = self.describe_audit(task_id).await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected DescribeAuditTask to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()
    }

    /// Register one X.509 device certificate.
    async fn create_certificate(&self, case: &str) {
        let reply = self
            .dispatch("POST", "/keys-and-certificate?setAsActive=true", None)
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected CreateKeysAndCertificate to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    /// Make the completion the clock owes `task_id` DUE, by moving the recorded instants
    /// into the past — the state a caller that waited out `AUDIT_SPAN` would find, without
    /// the test waiting for it.
    fn finish_audit(&self, task_id: &str) {
        let state = self.engine.state::<ComAmazonawsIotAwsiotserviceState>();
        let mut store = state.lock();
        let task = store
            .audit_tasks
            .get_mut(task_id)
            .expect("the started audit is on record");
        let started = SystemTime::now() - AUDIT_SPAN - Duration::from_secs(1);
        task.started_at = started;
        task.completed_at = started + AUDIT_SPAN;
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    /// The error code a restJson1 error response carries.
    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    /// The message a restJson1 error response carries.
    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }
}

/// One check entry of a report's `auditDetails`.
fn check_details(case: &str, report: &Value, check_name: &str) -> Value {
    report["auditDetails"]
        .get(check_name)
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: no {check_name} entry in {report}"))
}

/// RULE: whether the account HAS an audit configuration is read before any check the
/// request named, so an account that set none up is refused the account-level precondition
/// — and only an account that has one gets the per-check refusal.
#[tokio::test]
async fn an_account_with_no_audit_configuration_is_refused_before_its_checks_are_looked_at() {
    // The rows span what the rule generalises over: which operation targets the checks, and
    // what the request targeted — one check, several, a name outside the catalog of audit
    // checks, and no check at all. None of it is looked at while the account has no
    // configuration.
    let targets: &[(&str, &[&str])] = &[
        ("one catalog check", &[LOGGING_DISABLED_CHECK]),
        (
            "three catalog checks",
            &[
                LOGGING_DISABLED_CHECK,
                DEVICE_CERTIFICATE_AGE_CHECK,
                IOT_POLICY_OVERLY_PERMISSIVE_CHECK,
            ],
        ),
        ("a name outside the catalog", &["NOT_AN_AUDIT_CHECK"]),
        ("no check at all", &[]),
    ];

    for (row, (targeted, check_names)) in targets.iter().enumerate() {
        for operation in ["StartOnDemandAuditTask", "CreateScheduledAudit"] {
            let case = format!("{operation} targeting {targeted}");
            // A fresh emulator per row: an account that has never called
            // `UpdateAccountAuditConfiguration` is the condition under test.
            let emulator = Emulator::new();
            let reply = if operation == "StartOnDemandAuditTask" {
                emulator.start_audit(check_names).await
            } else {
                emulator
                    .create_scheduled_audit(&format!("nightly-{row}"), check_names)
                    .await
            };
            assert_eq!(
                reply.status,
                400,
                "case {case}: expected 400, got {} body {}",
                reply.status,
                reply.text()
            );
            assert_eq!(
                reply.error_code(),
                "InvalidRequestException",
                "case {case}: got {}",
                reply.text()
            );
            assert_eq!(
                reply.error_message(),
                ACCOUNT_NOT_SET_UP,
                "case {case}: expected the account-level precondition, got {}",
                reply.text()
            );
        }
    }

    // With the account set up, the SAME requests are judged on the checks they name: a
    // check the account has not enabled is the per-check refusal, and an enabled one runs.
    let emulator = Emulator::new();
    emulator
        .enable_checks("a configured account", &[LOGGING_DISABLED_CHECK])
        .await;

    let not_enabled = emulator.start_audit(&[DEVICE_CERTIFICATE_AGE_CHECK]).await;
    assert_eq!(
        not_enabled.status,
        400,
        "case a configured account targeting a check it has not enabled: expected 400, got {} \
         body {}",
        not_enabled.status,
        not_enabled.text()
    );
    assert_eq!(
        not_enabled.error_message(),
        format!("Audit check {DEVICE_CERTIFICATE_AGE_CHECK} is not enabled for this account"),
        "case a configured account targeting a check it has not enabled: expected the \
         per-check refusal, got {}",
        not_enabled.text()
    );

    let scheduled = emulator
        .create_scheduled_audit("nightly-enabled", &[LOGGING_DISABLED_CHECK])
        .await;
    assert_eq!(
        scheduled.status,
        200,
        "case a configured account scheduling an enabled check: expected 200, got {} body {}",
        scheduled.status,
        scheduled.text()
    );
    let started = emulator
        .started_audit("a configured account targeting its enabled check", &[
            LOGGING_DISABLED_CHECK,
        ])
        .await;
    assert!(
        !started.is_empty(),
        "case a configured account targeting its enabled check: expected a task id, got none"
    );
}

/// RULE: a `taskId` the account started no audit under is reported missing by one
/// sentence, whichever operation addressed the task and whatever form the id took.
#[tokio::test]
async fn a_task_id_no_audit_was_started_under_is_reported_missing_by_one_sentence() {
    let emulator = Emulator::new();
    // Ids of different forms, each one the modelled `AuditTaskId` admits (len 1..40,
    // pattern ^[a-zA-Z0-9\-]+$): the shortest, an opaque token of the width this service
    // mints, a hyphenated UUID, and the longest the model allows.
    let ids = [
        "a",
        "xYZUVCK0nLiqvBlWeanp",
        "6f1a9c3e-2b7d-4f80-9a1c-5d3e7b2f4a60",
        "0123456789012345678901234567890123456789",
    ];
    for task_id in ids {
        for (operation, reply) in [
            ("DescribeAuditTask", emulator.describe_audit(task_id).await),
            ("CancelAuditTask", emulator.cancel_audit(task_id).await),
        ] {
            let case = format!("{operation} on {task_id:?}");
            assert_eq!(
                reply.status,
                404,
                "case {case}: expected 404, got {} body {}",
                reply.status,
                reply.text()
            );
            assert_eq!(
                reply.error_code(),
                "ResourceNotFoundException",
                "case {case}: got {}",
                reply.text()
            );
            assert_eq!(
                reply.error_message(),
                format!("Audit task with TaskId={task_id} was not found"),
                "case {case}: expected the audit-task not-found sentence, got {}",
                reply.text()
            );
        }
    }
}

/// RULE: the id an on-demand audit is started under is a hyphen-free lowercase
/// hexadecimal token of fixed width — the shape class AWS identifies a Device Defender
/// audit task in — unique per audit, and the audit is readable back under it.
#[tokio::test]
async fn a_started_audit_is_identified_by_a_lowercase_hexadecimal_token() {
    let emulator = Emulator::new();
    let enabled = [
        LOGGING_DISABLED_CHECK,
        DEVICE_CERTIFICATE_AGE_CHECK,
        IOT_POLICY_OVERLY_PERMISSIVE_CHECK,
    ];
    emulator.enable_checks("minted ids", &enabled).await;

    // The rows differ along what the audit targeted, which nothing about the id depends
    // on; each row is started repeatedly, so the uniqueness and the shape are read over
    // many minted ids rather than one.
    let targets: &[(&str, &[&str])] = &[
        ("one check", &enabled[..1]),
        ("two checks", &enabled[..2]),
        ("every enabled check", &enabled),
    ];
    let mut minted: BTreeSet<String> = BTreeSet::new();
    for (targeted, check_names) in targets {
        for attempt in 0..8 {
            let case = format!("an audit over {targeted} (attempt {attempt})");
            let task_id = emulator.started_audit(&case, check_names).await;

            // The SHAPE CLASS, written out rather than read off the implementation, so a
            // change to how the id is minted cannot leave this row green: 32 characters of
            // lowercase hexadecimal, with no hyphen in them.
            assert_eq!(
                task_id.chars().count(),
                32,
                "case {case}: expected an id of 32 characters, got {task_id:?}"
            );
            assert!(
                task_id.chars().all(|character| character.is_ascii_digit()
                    || ('a'..='f').contains(&character)),
                "case {case}: expected a lowercase hexadecimal id, got {task_id:?}"
            );
            assert!(
                !task_id.contains('-'),
                "case {case}: expected an id with no hyphen in it, got {task_id:?}"
            );
            // Inside `AuditTaskId`'s modelled bound
            // (`smithy_query(query="shape",shape="AuditTaskId")`: len 1..40,
            // pattern ^[a-zA-Z0-9\-]+$).
            assert!(
                (1..=40).contains(&task_id.chars().count())
                    && task_id
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '-'),
                "case {case}: expected an id inside AuditTaskId's length and pattern, got \
                 {task_id:?}"
            );
            assert!(
                minted.insert(task_id.clone()),
                "case {case}: expected an id no earlier audit was started under, got the \
                 already-issued {task_id:?}"
            );

            // ROUND TRIP: the audit is readable back under exactly the id it answered.
            let report = emulator.reported_audit(&case, &task_id).await;
            assert_eq!(
                report["taskType"],
                json!("ON_DEMAND_AUDIT_TASK"),
                "case {case}: expected the audit started on demand, got {report}"
            );
        }
    }
}

/// RULE: every check entry of an audit carries the three resource counters — zero while
/// the check has performed nothing, and the counts of what it evaluated once it has run.
#[tokio::test]
async fn every_check_entry_reports_the_resource_counters() {
    // The rows differ along how many resources of the audited class the account holds, so
    // a counter that is zero because nothing was there is told apart from one that counts.
    let rows: &[(&str, usize)] = &[("an account with no certificate", 0), ("one certificate", 1)];
    let checks = [
        DEVICE_CERTIFICATE_EXPIRING_CHECK,
        DEVICE_CERTIFICATE_AGE_CHECK,
    ];

    for (case, certificates) in rows {
        let emulator = Emulator::new();
        emulator.enable_checks(case, &checks).await;
        for _ in 0..*certificates {
            emulator.create_certificate(case).await;
        }
        let task_id = emulator.started_audit(case, &checks).await;

        // IN PROGRESS: the counters are reported and all three are zero, and no compliance
        // verdict is stated for a check that has performed nothing.
        let running = emulator.reported_audit(case, &task_id).await;
        assert_eq!(
            running["taskStatus"],
            json!(AUDIT_TASK_IN_PROGRESS),
            "case {case}: a fresh audit must be in progress, got {running}"
        );
        for check_name in checks {
            let details = check_details(case, &running, check_name);
            for counter in RESOURCE_COUNTERS {
                assert_eq!(
                    details[counter].as_i64(),
                    Some(0),
                    "case {case}: expected {check_name}'s {counter} reported as 0 while the \
                     check is in progress, got {details}"
                );
            }
            assert!(
                details["checkCompliant"].is_null(),
                "case {case}: expected no compliance verdict for {check_name} while the check \
                 is in progress, got {details}"
            );
        }

        // RUN: the counters are what the check evaluated over the account's resources, and
        // nothing of this account suppresses a finding.
        emulator.finish_audit(&task_id);
        let report = emulator.reported_audit(case, &task_id).await;
        assert_eq!(
            report["taskStatus"],
            json!(AUDIT_TASK_COMPLETED),
            "case {case}: the audit's completion is due, got {report}"
        );
        for check_name in checks {
            let details = check_details(case, &report, check_name);
            assert_eq!(
                details["totalResourcesCount"].as_i64(),
                Some(i64::try_from(*certificates).expect("a row's count fits a long")),
                "case {case}: expected {check_name} to report the certificates it evaluated, \
                 got {details}"
            );
            assert_eq!(
                details["suppressedNonCompliantResourcesCount"].as_i64(),
                Some(0),
                "case {case}: nothing of this account suppresses a finding, got {details}"
            );
            assert!(
                details["nonCompliantResourcesCount"].as_i64().is_some(),
                "case {case}: expected {check_name} to report a non-compliant count, got \
                 {details}"
            );
            assert!(
                details["checkCompliant"].is_boolean(),
                "case {case}: expected a compliance verdict for the check that ran, got \
                 {details}"
            );
        }

        // CANCELLED: a check the cancel stopped performed no resource either, so it
        // reports the counters of one that performed none and no compliance verdict.
        let cancelled_case = format!("{case}, cancelled");
        let cancelled_task = emulator.started_audit(&cancelled_case, &checks).await;
        let cancelled = emulator.cancel_audit(&cancelled_task).await;
        assert_eq!(
            cancelled.status,
            200,
            "case {cancelled_case}: expected the cancel to answer 200, got {} body {}",
            cancelled.status,
            cancelled.text()
        );
        let after_cancel = emulator.reported_audit(&cancelled_case, &cancelled_task).await;
        for check_name in checks {
            let details = check_details(&cancelled_case, &after_cancel, check_name);
            for counter in RESOURCE_COUNTERS {
                assert_eq!(
                    details[counter].as_i64(),
                    Some(0),
                    "case {cancelled_case}: expected {check_name}'s {counter} reported as 0, \
                     got {details}"
                );
            }
            assert!(
                details["checkCompliant"].is_null(),
                "case {cancelled_case}: expected no compliance verdict for the cancelled \
                 {check_name}, got {details}"
            );
        }
    }
}
