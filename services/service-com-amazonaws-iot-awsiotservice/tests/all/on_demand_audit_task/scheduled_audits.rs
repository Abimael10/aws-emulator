// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of DEVICE DEFENDER SCHEDULED AUDITS: `CreateScheduledAudit`
//! and the `DescribeScheduledAudit` reader that proves what it persisted.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rules under test, none of them specific to one schedule:
//!
//! * a scheduled audit is a named, account-scoped record: the create persists, under the
//!   name the request line carries, the frequency, the day the schedule names and the
//!   checks it targets, mints the ARN
//!   `arn:aws:iot:<region>:<account>:scheduledaudit/<scheduledAuditName>` for it, and
//!   every later read of that name reports THAT record member for member;
//! * which DAY member the schedule must name follows from its frequency — `dayOfMonth` "is
//!   required if the frequency parameter is set to MONTHLY", `dayOfWeek` "is required if
//!   the frequency parameter is set to WEEKLY or BIWEEKLY", and a DAILY schedule names
//!   neither — and a schedule that leaves the demanded member out is refused
//!   `InvalidRequestException` (400), persisting nothing;
//! * "Checks must be enabled for your account": a request targeting no check, one outside
//!   the account's catalog of audit checks or one the account has not enabled is refused
//!   `InvalidRequestException` (400), persisting nothing;
//! * the name is the identity, so a second create under a name the account already holds a
//!   scheduled audit under is refused `ResourceAlreadyExistsException` (409) and leaves the
//!   stored schedule exactly as it was;
//! * a `scheduledAuditName` the account holds no scheduled audit under is
//!   `ResourceNotFoundException` (404) to the reader, never a hollow 200, and reading a
//!   schedule changes nothing a later reader sees;
//! * the tags a create carried are the resource's own, readable by its ARN through
//!   `ListTagsForResource`.
//!
//! The table spans what those rules generalise over — every modelled `AuditFrequency` and
//! every modelled `DayOfWeek` (read through the generated constants, so a value the model
//! gains is a case this suite demands), each published form of `DayOfMonth` including both
//! bounds of its pattern and `LAST`, how many checks a schedule targets, whether it
//! carries tags, and the form and length of the name within `ScheduledAuditName`'s
//! modelled 1..128 — rather than one recorded request.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_iot_awsiotservice::ops::types::{
    AUDIT_FREQUENCY_VALUES, DAY_OF_WEEK_VALUES,
};
use service_com_amazonaws_iot_awsiotservice::state::AUDIT_CHECK_NAMES;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The region this suite signs for; the ARN assertions read it back.
const REGION: &str = "ap-south-1";

/// The role the account's audit configuration names, of more than the modelled minimum
/// length (`RoleArn`, len 20..2048).
const AUDIT_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/device-defender-audit";

/// One audit check the account enables, named individually by the rows below. It is an
/// entry of the emulator's catalog of Device Defender audit checks
/// (`service_com_amazonaws_iot_awsiotservice::state::AUDIT_CHECK_NAMES`), which the rows
/// targeting `AUDIT_CHECK_NAMES` cover in full.
const LOGGING_DISABLED_CHECK: &str = "LOGGING_DISABLED_CHECK";
/// A second entry of that same catalog.
const DEVICE_CERTIFICATE_AGE_CHECK: &str = "DEVICE_CERTIFICATE_AGE_CHECK";
/// A third entry of that same catalog, used by the row that targets three checks and by
/// the refusal that targets a check the account has not enabled.
const CONFLICTING_CLIENT_IDS_CHECK: &str = "CONFLICTING_CLIENT_IDS_CHECK";

/// One row of the table: the schedule a create asks for.
struct Row {
    /// The `scheduledAuditName` the request line carries.
    name: String,
    /// The `frequency` the schedule runs at.
    frequency: &'static str,
    /// The `dayOfMonth` it names, for a frequency that demands one.
    day_of_month: Option<&'static str>,
    /// The `dayOfWeek` it names, for a frequency that demands one.
    day_of_week: Option<&'static str>,
    /// The checks it targets.
    targets: Vec<String>,
    /// The tags it carries, as `(key, value)` pairs.
    tags: Vec<(&'static str, &'static str)>,
}

/// The schedules this suite creates and reads back.
///
/// The rows differ along what the rules generalise over: the frequency (every modelled
/// `AuditFrequency`), the day the schedule names (both bounds of `DayOfMonth`'s pattern
/// `^([1-9]|[12][0-9]|3[01])$|^LAST$` and its `LAST` form; a day of the week), how many
/// checks are targeted (one, three, the whole catalog), whether tags are carried, and the
/// name's form and length within `ScheduledAuditName`'s modelled 1..128.
fn rows() -> Vec<Row> {
    vec![
        Row {
            name: "daily-audit".to_string(),
            frequency: "DAILY",
            day_of_month: None,
            day_of_week: None,
            targets: vec![LOGGING_DISABLED_CHECK.to_string()],
            tags: vec![],
        },
        Row {
            name: "weekly_audit_2".to_string(),
            frequency: "WEEKLY",
            day_of_month: None,
            day_of_week: Some("SUN"),
            targets: vec![
                LOGGING_DISABLED_CHECK.to_string(),
                DEVICE_CERTIFICATE_AGE_CHECK.to_string(),
            ],
            tags: vec![("stage", "prod")],
        },
        Row {
            name: "BIWEEKLY-audit-3".to_string(),
            frequency: "BIWEEKLY",
            day_of_month: None,
            day_of_week: Some("SAT"),
            targets: AUDIT_CHECK_NAMES.iter().map(|c| (*c).to_string()).collect(),
            tags: vec![("stage", "staging"), ("owner", "fleet-team")],
        },
        Row {
            // The first day of the month: the lower bound of `DayOfMonth`'s pattern.
            name: "monthly-first".to_string(),
            frequency: "MONTHLY",
            day_of_month: Some("1"),
            day_of_week: None,
            targets: vec![DEVICE_CERTIFICATE_AGE_CHECK.to_string()],
            tags: vec![],
        },
        Row {
            // The 31st: the upper bound of that pattern.
            name: "monthly_31".to_string(),
            frequency: "MONTHLY",
            day_of_month: Some("31"),
            day_of_week: None,
            targets: vec![
                LOGGING_DISABLED_CHECK.to_string(),
                DEVICE_CERTIFICATE_AGE_CHECK.to_string(),
                CONFLICTING_CLIENT_IDS_CHECK.to_string(),
            ],
            tags: vec![("probe", "")],
        },
        Row {
            // "LAST", the other form that pattern admits.
            name: "monthly-LAST".to_string(),
            frequency: "MONTHLY",
            day_of_month: Some("LAST"),
            day_of_week: None,
            targets: AUDIT_CHECK_NAMES.iter().map(|c| (*c).to_string()).collect(),
            tags: vec![("stage", "prod")],
        },
        Row {
            // The shortest name `ScheduledAuditName` admits (len min 1).
            name: "a".to_string(),
            frequency: "DAILY",
            day_of_month: None,
            day_of_week: None,
            targets: vec![CONFLICTING_CLIENT_IDS_CHECK.to_string()],
            tags: vec![],
        },
        Row {
            // The longest one it admits (len max 128).
            name: longest_name(),
            frequency: "WEEKLY",
            day_of_month: None,
            day_of_week: Some("WED"),
            targets: vec![LOGGING_DISABLED_CHECK.to_string()],
            tags: vec![],
        },
    ]
}

/// A name of `ScheduledAuditName`'s maximum modelled length (128), of characters its
/// pattern `^[a-zA-Z0-9_-]+$` admits.
fn longest_name() -> String {
    let prefix = "audit-";
    format!("{prefix}{}", "n".repeat(128 - prefix.len()))
}

/// The ARN this plane mints for `scheduled_audit_name`.
fn expected_arn(scheduled_audit_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:scheduledaudit/{scheduled_audit_name}")
}

/// How this service states that a create's name is already taken by a scheduled audit:
/// the by-name form this service refuses a Device Defender named resource's create with.
fn already_exists_message(scheduled_audit_name: &str) -> String {
    format!("ScheduledAudit cannot be created - name already exists (name={scheduled_audit_name})")
}

/// How this service states that a name names no scheduled audit this account holds.
fn not_found_message(scheduled_audit_name: &str) -> String {
    format!("Scheduled audit {scheduled_audit_name} does not exist.")
}

impl Row {
    /// The create request body this row asks for.
    fn body(&self) -> Value {
        let mut request = Map::new();
        request.insert("frequency".to_string(), json!(self.frequency));
        if let Some(day_of_month) = self.day_of_month {
            request.insert("dayOfMonth".to_string(), json!(day_of_month));
        }
        if let Some(day_of_week) = self.day_of_week {
            request.insert("dayOfWeek".to_string(), json!(day_of_week));
        }
        request.insert("targetCheckNames".to_string(), json!(self.targets));
        if !self.tags.is_empty() {
            let tags: Vec<Value> = self
                .tags
                .iter()
                .map(|(key, value)| json!({ "Key": key, "Value": value }))
                .collect();
            request.insert("tags".to_string(), Value::Array(tags));
        }
        Value::Object(request)
    }

    /// What `DescribeScheduledAudit` has to report for this row.
    fn expected_description(&self) -> Value {
        let mut expected = Map::new();
        expected.insert("scheduledAuditName".to_string(), json!(self.name));
        expected.insert(
            "scheduledAuditArn".to_string(),
            json!(expected_arn(&self.name)),
        );
        expected.insert("frequency".to_string(), json!(self.frequency));
        if let Some(day_of_month) = self.day_of_month {
            expected.insert("dayOfMonth".to_string(), json!(day_of_month));
        }
        if let Some(day_of_week) = self.day_of_week {
            expected.insert("dayOfWeek".to_string(), json!(day_of_week));
        }
        expected.insert("targetCheckNames".to_string(), json!(self.targets));
        Value::Object(expected)
    }
}

/// One emulator: the shared engine plus a router with this service mounted.
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

    /// An emulator whose account has enabled every audit check in the catalog, which is
    /// the state a scheduled audit may target checks in.
    async fn with_every_check_enabled() -> Self {
        let emulator = Self::new();
        emulator.enable_checks("setup", AUDIT_CHECK_NAMES).await;
        emulator
    }

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            // The credential scope names the `iot` signing name the model's
            // `aws.auth#sigv4` trait declares, which is what routes the request.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the scheduled-audit request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    /// Enable `check_names` for the account, which is what lets a scheduled audit target
    /// them.
    async fn enable_checks(&self, case: &str, check_names: &[&str]) {
        let mut checks = Map::new();
        for check_name in check_names {
            checks.insert((*check_name).to_string(), json!({ "enabled": true }));
        }
        let request = json!({
            "roleArn": AUDIT_ROLE_ARN,
            "auditCheckConfigurations": Value::Object(checks),
        });
        let reply = self
            .dispatch("PATCH", "/audit/configuration", Some(&request))
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected UpdateAccountAuditConfiguration to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    async fn create(&self, scheduled_audit_name: &str, body: &Value) -> Reply {
        self.dispatch(
            "POST",
            &format!("/audit/scheduledaudits/{scheduled_audit_name}"),
            Some(body),
        )
        .await
    }

    async fn describe(&self, scheduled_audit_name: &str) -> Reply {
        self.dispatch(
            "GET",
            &format!("/audit/scheduledaudits/{scheduled_audit_name}"),
            None,
        )
        .await
    }

    /// The tags of the resource `resource_arn` names, as `(key, value)` pairs.
    async fn tags_of(&self, case: &str, resource_arn: &str) -> Vec<(String, String)> {
        let encoded: String = ::form_urlencoded::Serializer::new(String::new())
            .append_pair("resourceArn", resource_arn)
            .finish();
        let reply = self.dispatch("GET", &format!("/tags?{encoded}"), None).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected ListTagsForResource to answer 200 for {resource_arn}, \
             got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["tags"]
            .as_array()
            .map(|tags| {
                tags.iter()
                    .map(|tag| {
                        (
                            tag["Key"].as_str().unwrap_or_default().to_string(),
                            tag["Value"].as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Create `row` and require it to succeed, answering the ARN it reported.
    async fn created(&self, row: &Row) -> String {
        let reply = self.create(&row.name, &row.body()).await;
        assert_eq!(
            reply.status, 200,
            "case {}: expected CreateScheduledAudit to answer 200, got {} body {}",
            row.name,
            reply.status,
            reply.text()
        );
        reply.json()["scheduledAuditArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {}: no scheduledAuditArn in {}", row.name, reply.text()))
            .to_string()
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

/// The table covers every modelled `AuditFrequency`, so a frequency the model gains is a
/// row this suite demands rather than a case nothing exercises.
#[test]
fn the_table_spans_every_modeled_audit_frequency() {
    let covered: std::collections::BTreeSet<&str> =
        rows().iter().map(|row| row.frequency).collect();
    let modeled: std::collections::BTreeSet<&str> = AUDIT_FREQUENCY_VALUES.iter().copied().collect();
    assert_eq!(
        covered, modeled,
        "expected one row per modelled AuditFrequency {AUDIT_FREQUENCY_VALUES:?}, got {covered:?}"
    );
}

/// RULE: a created scheduled audit is read back member for member, under the ARN this
/// plane mints for its name — whatever its frequency, the day it names, how many checks it
/// targets or how its name is spelled.
#[tokio::test]
async fn a_created_scheduled_audit_is_read_back_member_for_member() {
    let emulator = Emulator::with_every_check_enabled().await;

    for row in rows() {
        let reported_arn = emulator.created(&row).await;
        assert_eq!(
            reported_arn,
            expected_arn(&row.name),
            "case {}: expected the ARN this plane mints, got {reported_arn}",
            row.name
        );

        let reply = emulator.describe(&row.name).await;
        assert_eq!(
            reply.status, 200,
            "case {}: expected DescribeScheduledAudit to answer 200, got {} body {}",
            row.name,
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.json(),
            row.expected_description(),
            "case {}: expected the created schedule reported member for member, got {}",
            row.name,
            reply.text()
        );
    }
}

/// RULE: reading a schedule changes nothing a later caller sees, and the record is the one
/// the create persisted however often it is read.
#[tokio::test]
async fn reading_a_scheduled_audit_changes_nothing() {
    let emulator = Emulator::with_every_check_enabled().await;

    for row in rows() {
        emulator.created(&row).await;
        let first = emulator.describe(&row.name).await.json();
        let second = emulator.describe(&row.name).await.json();
        assert_eq!(
            first, second,
            "case {}: expected two reads of one schedule to report one record, got {first} \
             then {second}",
            row.name
        );
        assert_eq!(
            second,
            row.expected_description(),
            "case {}: expected the stored record, got {second}",
            row.name
        );
    }
}

/// RULE: every day of the week the model declares is a day a weekly schedule can run on,
/// and the day it was created with is the day it reports.
#[tokio::test]
async fn every_modeled_day_of_week_is_a_schedule_the_plane_persists() {
    let emulator = Emulator::with_every_check_enabled().await;

    for day_of_week in DAY_OF_WEEK_VALUES {
        let row = Row {
            name: format!("weekly-{day_of_week}"),
            frequency: "WEEKLY",
            day_of_month: None,
            day_of_week: Some(day_of_week),
            targets: vec![LOGGING_DISABLED_CHECK.to_string()],
            tags: vec![],
        };
        emulator.created(&row).await;
        let reported = emulator.describe(&row.name).await.json();
        assert_eq!(
            reported,
            row.expected_description(),
            "case {day_of_week}: expected the created schedule reported back, got {reported}"
        );
    }
}

/// RULE: which day member a schedule must name follows from its frequency, and a schedule
/// that leaves the demanded member out is refused and persists nothing.
#[tokio::test]
async fn a_schedule_that_names_no_day_its_frequency_demands_is_refused() {
    let emulator = Emulator::with_every_check_enabled().await;

    // One case per frequency that demands a day, each sending the OTHER day member (or
    // none) so what is missing is the member the frequency demands.
    let cases: Vec<(&str, &str, Option<&str>, &str)> = vec![
        ("monthly-no-day", "MONTHLY", None, "dayOfMonth"),
        ("monthly-wrong-day", "MONTHLY", Some("dayOfWeek"), "dayOfMonth"),
        ("weekly-no-day", "WEEKLY", None, "dayOfWeek"),
        ("weekly-wrong-day", "WEEKLY", Some("dayOfMonth"), "dayOfWeek"),
        ("biweekly-no-day", "BIWEEKLY", None, "dayOfWeek"),
        (
            "biweekly-wrong-day",
            "BIWEEKLY",
            Some("dayOfMonth"),
            "dayOfWeek",
        ),
    ];

    for (name, frequency, sent_member, missing_member) in cases {
        let mut request = Map::new();
        request.insert("frequency".to_string(), json!(frequency));
        request.insert(
            "targetCheckNames".to_string(),
            json!([LOGGING_DISABLED_CHECK]),
        );
        match sent_member {
            Some("dayOfMonth") => {
                request.insert("dayOfMonth".to_string(), json!("LAST"));
            }
            Some("dayOfWeek") => {
                request.insert("dayOfWeek".to_string(), json!("MON"));
            }
            _ => {}
        }
        let reply = emulator.create(name, &Value::Object(request)).await;
        assert_eq!(
            reply.status, 400,
            "case {name}: expected 400 for a {frequency} schedule without {missing_member}, \
             got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "InvalidRequestException",
            "case {name}: expected InvalidRequestException, got {}",
            reply.text()
        );
        assert!(
            reply.error_message().contains(missing_member)
                && reply.error_message().contains(frequency),
            "case {name}: expected the refusal to name {missing_member} and {frequency}, \
             got {:?}",
            reply.error_message()
        );

        // Nothing was persisted: the refused name names no scheduled audit.
        let read = emulator.describe(name).await;
        assert_eq!(
            read.status, 404,
            "case {name}: expected the refused schedule to persist nothing, got {} body {}",
            read.status,
            read.text()
        );
    }

    // A DAILY schedule demands neither day member, so it is created without one.
    let daily = Row {
        name: "daily-needs-no-day".to_string(),
        frequency: "DAILY",
        day_of_month: None,
        day_of_week: None,
        targets: vec![LOGGING_DISABLED_CHECK.to_string()],
        tags: vec![],
    };
    emulator.created(&daily).await;
}

/// RULE: a `dayOfMonth` outside what the member's modelled pattern admits is refused, one
/// step past each published bound of `^([1-9]|[12][0-9]|3[01])$|^LAST$`.
#[tokio::test]
async fn a_day_of_month_outside_the_modeled_pattern_is_refused() {
    let emulator = Emulator::with_every_check_enabled().await;

    for (name, day_of_month) in [
        ("monthly-zero", "0"),
        ("monthly-thirty-two", "32"),
        ("monthly-last-lowercase", "last"),
    ] {
        let request = json!({
            "frequency": "MONTHLY",
            "dayOfMonth": day_of_month,
            "targetCheckNames": [LOGGING_DISABLED_CHECK],
        });
        let reply = emulator.create(name, &request).await;
        assert_eq!(
            reply.status, 400,
            "case {name}: expected 400 for dayOfMonth {day_of_month:?}, got {} body {}",
            reply.status,
            reply.text()
        );
        let read = emulator.describe(name).await;
        assert_eq!(
            read.status, 404,
            "case {name}: expected the refused schedule to persist nothing, got {} body {}",
            read.status,
            read.text()
        );
    }
}

/// RULE: "Checks must be enabled for your account" — a target that names no check, a name
/// outside the catalog and a check the account has not enabled are all refused, and none
/// of them persists a schedule.
#[tokio::test]
async fn a_target_the_account_cannot_audit_is_refused() {
    let emulator = Emulator::new();
    // Every check EXCEPT one, so a target naming that one is a check the account holds a
    // catalog entry for but has not enabled.
    let enabled: Vec<&str> = AUDIT_CHECK_NAMES
        .iter()
        .copied()
        .filter(|check| *check != CONFLICTING_CLIENT_IDS_CHECK)
        .collect();
    emulator.enable_checks("setup", &enabled).await;

    let cases: Vec<(&str, Value)> = vec![
        ("targets-nothing", json!([])),
        ("targets-unknown-check", json!(["NO_SUCH_CHECK"])),
        (
            "targets-disabled-check",
            json!([CONFLICTING_CLIENT_IDS_CHECK]),
        ),
        (
            "targets-one-disabled-among-enabled",
            json!([LOGGING_DISABLED_CHECK, CONFLICTING_CLIENT_IDS_CHECK]),
        ),
    ];

    for (name, targets) in cases {
        let request = json!({ "frequency": "DAILY", "targetCheckNames": targets });
        let reply = emulator.create(name, &request).await;
        assert_eq!(
            reply.status, 400,
            "case {name}: expected 400 for targets {targets}, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "InvalidRequestException",
            "case {name}: expected InvalidRequestException, got {}",
            reply.text()
        );
        let read = emulator.describe(name).await;
        assert_eq!(
            read.status, 404,
            "case {name}: expected the refused schedule to persist nothing, got {} body {}",
            read.status,
            read.text()
        );
    }

    // The same request, once the check is enabled, is a schedule the account may run:
    // what refused it was the account's configuration and not the request's shape.
    emulator.enable_checks("enable-the-rest", AUDIT_CHECK_NAMES).await;
    let row = Row {
        name: "targets-disabled-check".to_string(),
        frequency: "DAILY",
        day_of_month: None,
        day_of_week: None,
        targets: vec![CONFLICTING_CLIENT_IDS_CHECK.to_string()],
        tags: vec![],
    };
    emulator.created(&row).await;
}

/// RULE: the name is the schedule's identity, so a second create under a name the account
/// already holds one under is a conflict that leaves the stored record untouched.
#[tokio::test]
async fn a_second_create_under_a_taken_name_is_refused_and_changes_nothing() {
    let emulator = Emulator::with_every_check_enabled().await;

    for row in rows() {
        emulator.created(&row).await;
        let stored = emulator.describe(&row.name).await.json();

        // A DIFFERENT schedule under the same name: the collision is the name, not the
        // schedule the second request asks for.
        let second = json!({
            "frequency": "MONTHLY",
            "dayOfMonth": "LAST",
            "targetCheckNames": [DEVICE_CERTIFICATE_AGE_CHECK],
        });
        let reply = emulator.create(&row.name, &second).await;
        assert_eq!(
            reply.status, 409,
            "case {}: expected 409 from the second create, got {} body {}",
            row.name,
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "ResourceAlreadyExistsException",
            "case {}: expected ResourceAlreadyExistsException, got {}",
            row.name,
            reply.text()
        );
        assert_eq!(
            reply.error_message(),
            already_exists_message(&row.name),
            "case {}: expected this family's already-exists sentence, got {:?}",
            row.name,
            reply.error_message()
        );
        assert_eq!(
            emulator.describe(&row.name).await.json(),
            stored,
            "case {}: expected the refused create to leave the stored schedule as it was",
            row.name
        );
    }
}

/// RULE: a name the account holds no scheduled audit under is a 404 naming the name asked
/// about, never a hollow 200 — including a name a refused create used and a name only a
/// different schedule's create took.
#[tokio::test]
async fn a_name_no_schedule_is_stored_under_is_not_found() {
    let emulator = Emulator::with_every_check_enabled().await;
    let stored = Row {
        name: "stored-audit".to_string(),
        frequency: "DAILY",
        day_of_month: None,
        day_of_week: None,
        targets: vec![LOGGING_DISABLED_CHECK.to_string()],
        tags: vec![],
    };
    emulator.created(&stored).await;

    for name in [
        "absent-audit",
        "STORED-AUDIT",
        "stored-audit-2",
        "a",
        longest_name().as_str(),
    ] {
        let reply = emulator.describe(name).await;
        assert_eq!(
            reply.status, 404,
            "case {name}: expected 404 for a name no schedule is stored under, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "ResourceNotFoundException",
            "case {name}: expected ResourceNotFoundException, got {}",
            reply.text()
        );
        assert_eq!(
            reply.error_message(),
            not_found_message(name),
            "case {name}: expected this family's not-found sentence, got {:?}",
            reply.error_message()
        );
    }
}

/// RULE: a check named twice is one check of the schedule, so what a reader reports is
/// each distinct check the request targeted, in the order it first named it.
#[tokio::test]
async fn a_check_named_twice_is_one_check_of_the_schedule() {
    let emulator = Emulator::with_every_check_enabled().await;

    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "one-name-twice",
            json!([LOGGING_DISABLED_CHECK, LOGGING_DISABLED_CHECK]),
            vec![LOGGING_DISABLED_CHECK],
        ),
        (
            "two-names-interleaved",
            json!([
                DEVICE_CERTIFICATE_AGE_CHECK,
                LOGGING_DISABLED_CHECK,
                DEVICE_CERTIFICATE_AGE_CHECK,
            ]),
            vec![DEVICE_CERTIFICATE_AGE_CHECK, LOGGING_DISABLED_CHECK],
        ),
        (
            "every-check-twice",
            json!(AUDIT_CHECK_NAMES
                .iter()
                .chain(AUDIT_CHECK_NAMES.iter())
                .collect::<Vec<_>>()),
            AUDIT_CHECK_NAMES.to_vec(),
        ),
    ];

    for (name, targets, expected) in cases {
        let request = json!({ "frequency": "DAILY", "targetCheckNames": targets });
        let reply = emulator.create(name, &request).await;
        assert_eq!(
            reply.status, 200,
            "case {name}: expected 200 for targets {targets}, got {} body {}",
            reply.status,
            reply.text()
        );
        let reported = emulator.describe(name).await.json();
        assert_eq!(
            reported["targetCheckNames"],
            json!(expected),
            "case {name}: expected each distinct check once, got {reported}"
        );
    }
}

/// RULE: the tags a create carried are the resource's own, read back by the ARN the create
/// reported.
#[tokio::test]
async fn the_tags_a_create_carried_are_the_resources_own() {
    let emulator = Emulator::with_every_check_enabled().await;

    for row in rows() {
        let arn = emulator.created(&row).await;
        let reported = emulator.tags_of(&row.name, &arn).await;
        let mut expected: Vec<(String, String)> = row
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        expected.sort();
        assert_eq!(
            reported, expected,
            "case {}: expected the tags the create carried, got {reported:?}",
            row.name
        );
    }
}
