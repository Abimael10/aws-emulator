// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of how a constraint refusal of this service STATES a `@pattern`
//! requirement: WHICH expression the clause quotes, and WHETHER it quotes the value that
//! missed it.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK client
//! would see, not an internal function's return value.
//!
//! The two rules under test, neither of them specific to one operation or one request:
//!
//! * the expression a clause quotes is the one this service publishes for the member's SHAPE:
//!   the model's text without the whole-value anchors it wraps the expression in, EXCEPT
//!   where what that pair encloses carries an anchor of its own — an alternation whose
//!   branches are each anchored — which is already a whole-value expression before any
//!   wrapping and is quoted as the model declares it, anchors included. Dropping the pair
//!   from such a text would state a requirement of neither branch, so a caller compiling the
//!   quoted expression would admit values this service refuses;
//! * which half of the clause states a value is reference data about that same shape: the
//!   expressions this service publishes WITH the value that missed them state it,
//!   single-quoted, and every other expression is named alone.
//!
//! The table spans what those rules generalise over rather than one recorded request: two
//! expression forms (one whose branches anchor themselves, three the model merely wraps),
//! both halves of the value decision, two operations carrying the SAME `@pattern`-constrained
//! shape (`CreateScheduledAudit` and `UpdateScheduledAudit`, whose `dayOfMonth` both target
//! `com.amazonaws.iot#DayOfMonth`), two members of ONE operation that take opposite halves of
//! the value decision, a member bound to the body and members bound to the URI, and several
//! rejected values per member — a step past each published bound of the day-of-month
//! expression, a wrongly cased alternative, a leading zero and a trailing space. The round
//! trip at the end is the other side of the same decision: each form that expression ADMITS
//! is created and read back, so the refusals are about the values the requests sent and not
//! about the operations.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_iot_awsiotservice::state::AUDIT_CHECK_NAMES;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite is signed for. No refusal here is regional, so no
/// assertion depends on it.
const REGION: &str = "us-east-2";

/// `InvalidRequestException` as AWS IoT names it in `x-amzn-errortype`: the shape name, a
/// `:`, and the service's API doc namespace.
const QUALIFIED_INVALID_REQUEST: &str =
    "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// The expression `com.amazonaws.iot#DayOfMonth` carries, as this service publishes it: the
/// model's own text (`smithy_query(query="shape",shape="DayOfMonth")` reports
/// `^([1-9]|[12][0-9]|3[01])$|^LAST$`), because its two branches each carry the anchors that
/// make this a whole-value match.
const DAY_OF_MONTH_EXPRESSION: &str = "^([1-9]|[12][0-9]|3[01])$|^LAST$";

/// The expression `com.amazonaws.iot#ScheduledAuditName` carries, as this service publishes
/// it: the model's `^[a-zA-Z0-9_-]+$` (`smithy_query(query="shape",shape="ScheduledAuditName")`)
/// without the whole-value anchors wrapped around it.
const SCHEDULED_AUDIT_NAME_EXPRESSION: &str = "[a-zA-Z0-9_-]+";

/// The expression the thing registry's name types carry, as this service publishes it: the
/// model's `^[a-zA-Z0-9:_-]+$` (`smithy_query(query="shape",shape="ThingName")`) without
/// those anchors.
const REGISTRY_NAME_EXPRESSION: &str = "[a-zA-Z0-9:_-]+";

/// The expression `com.amazonaws.iot#DomainConfigurationName` carries, as this service
/// publishes it: the model's `^[\w.-]+$`
/// (`smithy_query(query="shape",shape="DomainConfigurationName")`) without those anchors.
const DOMAIN_CONFIGURATION_NAME_EXPRESSION: &str = r"[\w.-]+";

/// One entry of this emulator's catalog of Device Defender audit checks
/// (`service_com_amazonaws_iot_awsiotservice::state::AUDIT_CHECK_NAMES`), which a schedule
/// targets.
const LOGGING_DISABLED_CHECK: &str = "LOGGING_DISABLED_CHECK";

/// The role the account's audit configuration names, of more than the modelled minimum length
/// (`RoleArn`, len 20..2048).
const AUDIT_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/device-defender-audit";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name: the
/// scope is how the router picks the claiming service. The signature itself is never checked
/// here — the harness turns request authentication off, exactly as an unauthenticated
/// emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// A URI path segment carrying `value`: a character a path may not hold travels
/// percent-encoded, as any client sends it, and what the clause states is the value the
/// service read back.
fn path_segment(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            ' ' => "%20".to_string(),
            '+' => "%2B".to_string(),
            '!' | '?' | '#' | '&' | '/' => format!("%{:02X}", character as u32),
            other => other.to_string(),
        })
        .collect()
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The `x-amzn-errortype` header, verbatim — including any namespace the shape name is
    /// qualified with.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` the body carries, or `""` when it carries none.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached the
/// way the production router reaches it.
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

    /// An emulator whose account has enabled every audit check in the catalog, which is the
    /// state a scheduled audit may target checks in.
    async fn with_every_check_enabled() -> Self {
        let emulator = Self::new();
        let mut checks = Map::new();
        for check_name in AUDIT_CHECK_NAMES {
            checks.insert((*check_name).to_string(), json!({ "enabled": true }));
        }
        let request = json!({
            "roleArn": AUDIT_ROLE_ARN,
            "auditCheckConfigurations": Value::Object(checks),
        });
        let reply = emulator
            .call("PATCH", "/audit/configuration", Some(&request))
            .await;
        assert_eq!(
            reply.status, 200,
            "setup: expected UpdateAccountAuditConfiguration to answer 200, got {} {:?}",
            reply.status, reply.body
        );
        emulator
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Bytes::new, |value| {
            Bytes::from(serde_json::to_vec(value).expect("a request body serializes"))
        });
        let host = format!("iot.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(payload)
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
}

/// One request refused for a member outside its `@pattern`, and what the clause must state.
struct PatternRefusal {
    case: String,
    method: &'static str,
    path: String,
    body: Option<Value>,
    /// The input member the value sat at, as the wire spells it.
    member: &'static str,
    /// What the request sent for that member.
    rejected: String,
    /// The expression the clause has to quote.
    expression: &'static str,
    /// Whether the clause quotes the rejected value.
    quotes_the_value: bool,
}

impl PatternRefusal {
    /// The whole message this refusal must carry: AWS's counted envelope over the one clause
    /// the request earned.
    fn expected_message(&self) -> String {
        let requirement = format!(
            "failed to satisfy constraint: Member must satisfy regular expression pattern: {}",
            self.expression
        );
        if self.quotes_the_value {
            format!(
                "1 validation error detected: Value '{}' at '{}' {requirement}",
                self.rejected, self.member
            )
        } else {
            format!(
                "1 validation error detected: Value at '{}' {requirement}",
                self.member
            )
        }
    }
}

/// The body a `CreateScheduledAudit` carries for a MONTHLY schedule naming `day_of_month`:
/// everything but that member satisfies the model, so a refused request earns exactly one
/// clause.
fn monthly_schedule(day_of_month: &str) -> Value {
    json!({
        "frequency": "MONTHLY",
        "dayOfMonth": day_of_month,
        "targetCheckNames": [LOGGING_DISABLED_CHECK],
    })
}

/// Every row of the table.
fn pattern_refusals() -> Vec<PatternRefusal> {
    let mut rows = Vec::new();

    // A `dayOfMonth` the expression refuses, over the two operations whose model gives that
    // member the same type: one step past each published bound (`0`, `32`), a value well past
    // the upper one, a leading zero, the wrongly cased alternative and one carrying a trailing
    // space.
    let rejected_days = ["0", "32", "99", "01", "last", "LAST "];
    let scheduling: [(&str, &str, bool); 2] = [
        ("CreateScheduledAudit", "POST", true),
        ("UpdateScheduledAudit", "PATCH", false),
    ];
    for (operation, method, carries_targets) in scheduling {
        for rejected in rejected_days {
            let mut body = monthly_schedule(rejected);
            if !carries_targets {
                // The update models no required member beyond the name, so it names the
                // frequency and the day alone.
                body.as_object_mut()
                    .expect("the schedule body is an object")
                    .remove("targetCheckNames");
            }
            rows.push(PatternRefusal {
                case: format!("{operation} on the day of month {rejected:?}"),
                method,
                path: "/audit/scheduledaudits/pattern-probe".to_string(),
                body: Some(body),
                member: "dayOfMonth",
                rejected: rejected.to_string(),
                expression: DAY_OF_MONTH_EXPRESSION,
                quotes_the_value: true,
            });
        }
    }

    // The OTHER `@pattern`-constrained member of the same operation: its expression is one
    // the model merely wraps, so the clause names it without the anchors — and this service
    // publishes it without the name that missed it.
    for rejected in ["bad name", "bad.name"] {
        rows.push(PatternRefusal {
            case: format!("CreateScheduledAudit named {rejected:?}"),
            method: "POST",
            path: format!("/audit/scheduledaudits/{}", path_segment(rejected)),
            body: Some(json!({
                "frequency": "DAILY",
                "targetCheckNames": [LOGGING_DISABLED_CHECK],
            })),
            member: "scheduledAuditName",
            rejected: rejected.to_string(),
            expression: SCHEDULED_AUDIT_NAME_EXPRESSION,
            quotes_the_value: false,
        });
    }

    // A THING-REGISTRY name: another expression the model merely wraps, and one this service
    // publishes WITH the name that missed it — the other half of the value decision for the
    // same rendering of the expression.
    for rejected in ["probe name", "probe.name"] {
        rows.push(PatternRefusal {
            case: format!("CreateThing named {rejected:?}"),
            method: "POST",
            path: format!("/things/{}", path_segment(rejected)),
            body: Some(json!({})),
            member: "thingName",
            rejected: rejected.to_string(),
            expression: REGISTRY_NAME_EXPRESSION,
            quotes_the_value: true,
        });
    }

    // A domain-configuration name, whose expression this service publishes without the value:
    // a third wrapped expression, on a third plane.
    for rejected in ["IoT:reserved", "config!"] {
        rows.push(PatternRefusal {
            case: format!("CreateDomainConfiguration named {rejected:?}"),
            method: "POST",
            path: format!("/domainConfigurations/{}", path_segment(rejected)),
            body: Some(json!({})),
            member: "domainConfigurationName",
            rejected: rejected.to_string(),
            expression: DOMAIN_CONFIGURATION_NAME_EXPRESSION,
            quotes_the_value: false,
        });
    }

    rows
}

/// RULE: a `@pattern` clause quotes the expression this service publishes for the member's
/// shape — the model's text less the whole-value anchors, or the model's text as declared
/// where what those anchors enclose anchors its own branches — and states the value that
/// missed it exactly for the expressions this service publishes with that value.
#[tokio::test]
async fn a_pattern_clause_quotes_the_published_expression_and_its_published_value_half() {
    for case in pattern_refusals() {
        // A fresh emulator per row: a validation refusal is decided by the REQUEST, so it
        // must not depend on what another row left behind.
        let emulator = Emulator::new();
        let reply = emulator
            .call(case.method, &case.path, case.body.as_ref())
            .await;
        assert_eq!(
            reply.status, 400,
            "case {:?}: expected status 400, got {} {:?}",
            case.case, reply.status, reply.body
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some(QUALIFIED_INVALID_REQUEST),
            "case {:?}: expected x-amzn-errortype {QUALIFIED_INVALID_REQUEST:?}, got {:?}",
            case.case,
            reply.error_type
        );
        let expected = case.expected_message();
        assert_eq!(
            reply.message(),
            expected,
            "case {:?}: expected message {expected:?}, got {:?}",
            case.case,
            reply.message()
        );
        // The two halves again, stated as a client reads them: the expression is quoted with
        // every character the model wrote for it, and the value is quoted only where this
        // service publishes it.
        let quoted_expression = format!("regular expression pattern: {}", case.expression);
        assert!(
            reply.message().ends_with(&quoted_expression),
            "case {:?}: expected the clause to end in {quoted_expression:?}, got {:?}",
            case.case,
            reply.message()
        );
        let quoted_value = format!("Value '{}'", case.rejected);
        assert_eq!(
            reply.message().contains(&quoted_value),
            case.quotes_the_value,
            "case {:?}: expected the clause to quote the submitted value: {}, got {:?}",
            case.case,
            case.quotes_the_value,
            reply.message()
        );
    }
}

/// A refusal is no write: the name a pattern-refused create named holds no scheduled audit
/// afterwards, so the clauses above are about the requests and not about a record one of them
/// left behind.
#[tokio::test]
async fn a_pattern_refused_create_persists_nothing() {
    let emulator = Emulator::with_every_check_enabled().await;
    for rejected in ["0", "32", "LAST "] {
        let name = "refused-schedule";
        let reply = emulator
            .call(
                "POST",
                &format!("/audit/scheduledaudits/{name}"),
                Some(&monthly_schedule(rejected)),
            )
            .await;
        assert_eq!(
            reply.status, 400,
            "case {rejected:?}: expected status 400, got {} {:?}",
            reply.status, reply.body
        );
        let read = emulator
            .call("GET", &format!("/audit/scheduledaudits/{name}"), None)
            .await;
        assert_eq!(
            read.status, 404,
            "case {rejected:?}: expected the refused schedule to persist nothing, got {} {:?}",
            read.status, read.body
        );
    }
}

/// The other side of the same decision: every form the day-of-month expression ADMITS — each
/// published bound of its numeric branch, an ordinary day inside it and its `LAST` branch — is
/// created and read back, so no clause above is about the operation.
#[tokio::test]
async fn a_day_of_month_the_expression_admits_is_created_and_read_back() {
    let emulator = Emulator::with_every_check_enabled().await;
    for (index, day_of_month) in ["1", "9", "28", "31", "LAST"].into_iter().enumerate() {
        let name = format!("admitted-{index}");
        let created = emulator
            .call(
                "POST",
                &format!("/audit/scheduledaudits/{name}"),
                Some(&monthly_schedule(day_of_month)),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {day_of_month:?}: expected the create to be accepted, got {} {:?}",
            created.status, created.body
        );
        let read = emulator
            .call("GET", &format!("/audit/scheduledaudits/{name}"), None)
            .await;
        assert_eq!(
            read.status, 200,
            "case {day_of_month:?}: expected the schedule to be readable, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body.get("dayOfMonth").and_then(Value::as_str),
            Some(day_of_month),
            "case {day_of_month:?}: expected the day to be read back, got {:?}",
            read.body
        );
    }
}
