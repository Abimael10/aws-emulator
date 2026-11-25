// SPDX-License-Identifier: CC0-1.0

//! Addressing an event bus by a name in the namespace EventBridge reserves for
//! the buses it names itself, end to end over the wire (`awsJson1_1`, `POST /`
//! with `X-Amz-Target: AWSEvents.<Operation>`) through the real
//! `MultiServiceRouter`.
//!
//! The RULE asserted here: a request that addresses an event bus by a submitted
//! NAME is refused as invalid input — HTTP 400, `ValidationException`, "EventBus
//! name starting with 'aws.' is not valid." — whenever that name lies in the
//! reserved namespace, before the store is consulted, because no bus of this
//! account can carry such a name; the partner event bus form is the one
//! exception, so a name of that form is resolved against the store like any
//! other and answered as present or missing.
//!
//! The tables span the class rather than one recorded value: the name rows
//! differ along where the reserved prefix sits, what follows it, how many
//! segments a partner-shaped name has, and whether the name arrives bare or
//! inside an event-bus ARN; the operation rows span every operation of this
//! service that resolves a bus from a submitted name — a read, the two bus
//! writes, the five rule operations, the three target operations and the two
//! permission operations — each driven with the name forms its own modelled
//! member admits.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::ComAmazonawsEventbridgeAwseventsState;
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "eu-west-2";

/// The error code a refusal of this class carries on the wire.
const VALIDATION: &str = "ValidationException";

/// The code a request naming a bus that merely does not exist carries, from
/// `smithy_query(query="operation", operation="DescribeEventBus")`.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The whole refusal message. It is FIXED text: the verdict is on the namespace
/// the name lies in, so it interpolates nothing from the request or the store.
/// Spelled out here rather than imported, so a wrong wording in the
/// implementation cannot make this pass.
const RESERVED_MESSAGE: &str = "EventBus name starting with 'aws.' is not valid.";

/// The emulator: one router carrying only this service over one shared engine,
/// plus that engine, so a test can read the store the handlers wrote.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    Emulator { router, engine }
}

/// This emulator's account id — the account segment of every ARN it mints.
fn account() -> &'static str {
    engine::emulator_account_id()
}

async fn call(emulator: &Emulator, operation: &str, body: &Value) -> (u16, Value) {
    let credential = format!(
        "AKIDTESTTESTTEST/20260101/{REGION}/events/aws4_request, \
         SignedHeaders=host, Signature=0000"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!("AWS4-HMAC-SHA256 Credential={credential}"),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, parsed)
}

/// The wire error code (`__type`), without its namespace; the empty string on a
/// success.
fn error_code(body: &Value) -> &str {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
}

/// The `message` an error body carries.
fn message(body: &Value) -> &str {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// `arn:aws:events:<region>:<account>:event-bus/<name>` — built here rather than
/// read from the implementation.
fn event_bus_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{}:event-bus/{name}", account())
}

/// The names of every bus the store holds, in its own order.
fn stored_buses(emulator: &Emulator) -> Vec<String> {
    emulator
        .engine
        .state::<ComAmazonawsEventbridgeAwseventsState>()
        .lock()
        .buses
        .keys()
        .map(|key| key.name.clone())
        .collect()
}

/// The ARN of the queue the target-valued requests below name.
fn queue_arn() -> String {
    format!("arn:aws:sqs:{REGION}:{}:reserved-namespace-probe", account())
}

/// The bus NAME a submitted value addresses: the whole remainder after an
/// event-bus ARN's resource-type segment, and the value itself for a bare name —
/// `EventBusNameOrArn`'s two forms.
fn addressed_bus(submitted: &str) -> &str {
    match submitted.split_once(":event-bus/") {
        Some((prefix, name)) if prefix.starts_with("arn:") => name,
        _ => submitted,
    }
}

/// The FORM a bus name arrives in, which is what decides whether a given
/// modelled member can carry it at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// A bare name carrying no `/`.
    Bare,
    /// A bare name carrying the `/` only a partner event bus name may hold.
    Slashed,
    /// An event-bus ARN wrapping the name.
    Arn,
}

/// The modelled shape the bus name travels in, per operation. Each admits a
/// different set of [`Form`]s, from `smithy_query(query="operation", ...)`:
/// `EventBusNameOrArn` (pattern
/// `^(arn:aws[\w-]*:events:[a-z]+-[a-z]+-[\w-]+:[0-9]{12}:event-bus\/)?[/\.\-_A-Za-z0-9]+$`)
/// admits every form, `EventBusName` (`^[/\.\-_A-Za-z0-9]+$`) admits no ARN, and
/// `NonPartnerEventBusName` (`^[\.\-_A-Za-z0-9]+$`) admits neither an ARN nor a
/// `/`.
#[derive(Clone, Copy, Debug)]
enum Member {
    NameOrArn,
    BusName,
    NonPartnerName,
}

impl Member {
    fn admits(self, form: Form) -> bool {
        match self {
            Self::NameOrArn => true,
            Self::BusName => form != Form::Arn,
            Self::NonPartnerName => form == Form::Bare,
        }
    }
}

/// One operation that resolves an event bus from a name the request submitted.
struct Addressing {
    operation: &'static str,
    member: Member,
    /// The request body, with the bus name written into the member it travels in.
    body: fn(&str) -> Value,
}

/// Every operation of this service whose request addresses a bus by a submitted
/// name: the bus read and its two writes, the rule operations, the target
/// operations and the permission operations. Each body carries whatever else its
/// model requires, so the only thing wrong with a refused request is the name.
fn addressings() -> Vec<Addressing> {
    vec![
        Addressing {
            operation: "DescribeEventBus",
            member: Member::NameOrArn,
            body: |bus| json!({ "Name": bus }),
        },
        Addressing {
            operation: "UpdateEventBus",
            member: Member::BusName,
            body: |bus| json!({ "Name": bus, "Description": "updated" }),
        },
        Addressing {
            operation: "DeleteEventBus",
            member: Member::BusName,
            body: |bus| json!({ "Name": bus }),
        },
        Addressing {
            operation: "PutRule",
            member: Member::NameOrArn,
            body: |bus| {
                json!({
                    "Name": "probe-rule",
                    "EventBusName": bus,
                    "EventPattern": r#"{"source":["com.demo.widgets"]}"#,
                })
            },
        },
        Addressing {
            operation: "DescribeRule",
            member: Member::NameOrArn,
            body: |bus| json!({ "Name": "probe-rule", "EventBusName": bus }),
        },
        Addressing {
            operation: "EnableRule",
            member: Member::NameOrArn,
            body: |bus| json!({ "Name": "probe-rule", "EventBusName": bus }),
        },
        Addressing {
            operation: "DisableRule",
            member: Member::NameOrArn,
            body: |bus| json!({ "Name": "probe-rule", "EventBusName": bus }),
        },
        Addressing {
            operation: "DeleteRule",
            member: Member::NameOrArn,
            body: |bus| json!({ "Name": "probe-rule", "EventBusName": bus }),
        },
        Addressing {
            operation: "ListRules",
            member: Member::NameOrArn,
            body: |bus| json!({ "EventBusName": bus }),
        },
        Addressing {
            operation: "ListRuleNamesByTarget",
            member: Member::NameOrArn,
            body: |bus| json!({ "TargetArn": queue_arn(), "EventBusName": bus }),
        },
        Addressing {
            operation: "ListTargetsByRule",
            member: Member::NameOrArn,
            body: |bus| json!({ "Rule": "probe-rule", "EventBusName": bus }),
        },
        Addressing {
            operation: "PutTargets",
            member: Member::NameOrArn,
            body: |bus| {
                json!({
                    "Rule": "probe-rule",
                    "EventBusName": bus,
                    "Targets": [{ "Id": "queue", "Arn": queue_arn() }],
                })
            },
        },
        Addressing {
            operation: "RemoveTargets",
            member: Member::NameOrArn,
            body: |bus| {
                json!({ "Rule": "probe-rule", "EventBusName": bus, "Ids": ["queue"] })
            },
        },
        Addressing {
            operation: "PutPermission",
            member: Member::NonPartnerName,
            body: |bus| {
                json!({
                    "EventBusName": bus,
                    "Action": "events:PutEvents",
                    "Principal": "123456789012",
                    "StatementId": "probe",
                })
            },
        },
        Addressing {
            operation: "RemovePermission",
            member: Member::NonPartnerName,
            body: |bus| json!({ "EventBusName": bus, "RemoveAllPermissions": true }),
        },
    ]
}

/// One name row: a label for the assertion messages, the name as the request
/// carries it, the form it arrives in, and whether it lies in the reserved
/// namespace as a bus name.
struct NameCase {
    label: &'static str,
    name: String,
    form: Form,
    reserved: bool,
}

/// The name table. Reserved rows differ along what follows the prefix (a
/// service-style label, nothing at all, a deeper label, the partner prefix
/// without its source segments, a partner-shaped name one segment short of the
/// modelled `EventSourceName` pattern `^aws\.partner(/[\.\-_A-Za-z0-9]+){2,}$`)
/// and along arriving inside an ARN; accepted rows differ along where `aws`
/// appears in a name that is NOT in the namespace, and carry the partner event
/// bus form bare and in its ARN, which the rule exempts.
fn name_cases() -> Vec<NameCase> {
    vec![
        NameCase {
            label: "a service-style reserved name",
            name: "aws.mybus".to_string(),
            form: Form::Bare,
            reserved: true,
        },
        NameCase {
            label: "the reserved prefix alone",
            name: "aws.".to_string(),
            form: Form::Bare,
            reserved: true,
        },
        NameCase {
            label: "a deeper reserved name",
            name: "aws.not-a-real-service".to_string(),
            form: Form::Bare,
            reserved: true,
        },
        NameCase {
            label: "the partner prefix with no source segments",
            name: "aws.partner".to_string(),
            form: Form::Bare,
            reserved: true,
        },
        NameCase {
            label: "a partner-shaped name one segment short",
            name: "aws.partner/onlyone".to_string(),
            form: Form::Slashed,
            reserved: true,
        },
        NameCase {
            label: "a reserved name inside an event-bus ARN",
            name: event_bus_arn("aws.mybus"),
            form: Form::Arn,
            reserved: true,
        },
        NameCase {
            label: "the partner event bus form, which no bus here carries",
            name: "aws.partner/example.com/1234/stream".to_string(),
            form: Form::Slashed,
            reserved: false,
        },
        NameCase {
            label: "the partner event bus form inside an event-bus ARN",
            name: event_bus_arn("aws.partner/example.com/1234/stream"),
            form: Form::Arn,
            reserved: false,
        },
        NameCase {
            label: "the prefix without its dot",
            name: "aws-bus".to_string(),
            form: Form::Bare,
            reserved: false,
        },
        NameCase {
            label: "a name merely starting with aws",
            name: "awsome-bus".to_string(),
            form: Form::Bare,
            reserved: false,
        },
        NameCase {
            label: "the prefix inside the name",
            name: "my.aws.bus".to_string(),
            form: Form::Bare,
            reserved: false,
        },
    ]
}

/// Every operation that resolves a bus by name refuses a name in the reserved
/// namespace the same way, with the same fixed sentence — and refuses nothing
/// else: a name that merely resembles the prefix, and the partner event bus form,
/// reach the store and are answered on what it holds.
#[tokio::test]
async fn every_operation_that_addresses_a_bus_by_name_refuses_the_reserved_namespace() {
    for case in name_cases() {
        for addressing in addressings() {
            if !addressing.member.admits(case.form) {
                continue;
            }
            let emulator = emulator();
            let before = stored_buses(&emulator);
            let operation = addressing.operation;
            let label = case.label;
            let name = case.name.as_str();
            let (status, body) = call(&emulator, operation, &(addressing.body)(name)).await;

            if case.reserved {
                assert_eq!(
                    status, 400,
                    "case {label:?} name {name:?} operation {operation}: expected a refusal, \
                     got {status} {body}"
                );
                assert_eq!(
                    error_code(&body),
                    VALIDATION,
                    "case {label:?} name {name:?} operation {operation}: expected \
                     {VALIDATION}, got {body}"
                );
                let actual = message(&body);
                assert_eq!(
                    actual, RESERVED_MESSAGE,
                    "case {label:?} name {name:?} operation {operation}: expected \
                     {RESERVED_MESSAGE:?}, got {actual:?}"
                );
            } else {
                let code = error_code(&body);
                assert!(
                    code.is_empty() || code == NOT_FOUND,
                    "case {label:?} name {name:?} operation {operation}: a name outside the \
                     reserved namespace is answered on what the store holds, got {status} {body}"
                );
                assert_ne!(
                    message(&body),
                    RESERVED_MESSAGE,
                    "case {label:?} name {name:?} operation {operation}: expected no \
                     reserved-namespace refusal, got {body}"
                );
            }

            // Whatever the verdict, no operation above brought the addressed bus
            // into being; a reserved name is refused before the store is touched
            // at all, so the records are exactly as they were found.
            let after = stored_buses(&emulator);
            assert!(
                !after.contains(&addressed_bus(name).to_string()),
                "case {label:?} name {name:?} operation {operation}: no bus may be created \
                 under the addressed name, got {after:?}"
            );
            if case.reserved {
                assert_eq!(
                    after, before,
                    "case {label:?} name {name:?} operation {operation}: the refusal must \
                     precede every store write"
                );
            }
        }
    }
}

/// The refusal is reported as the INPUT error it is, not as a missing resource:
/// a reserved name answers `ValidationException` while a name outside the
/// namespace that no bus carries answers `ResourceNotFoundException` with the
/// not-found sentence for that name — the distinction a client branches on.
#[tokio::test]
async fn a_reserved_name_is_an_input_error_while_an_absent_one_is_a_missing_resource() {
    let emulator = emulator();
    // (case, the Name, the code and message it answers)
    let rows: [(&str, String, &str, String); 4] = [
        (
            "a reserved name",
            "aws.mybus".to_string(),
            VALIDATION,
            RESERVED_MESSAGE.to_string(),
        ),
        (
            "a reserved name inside an event-bus ARN",
            event_bus_arn("aws.mybus"),
            VALIDATION,
            RESERVED_MESSAGE.to_string(),
        ),
        (
            "a name outside the namespace that no bus carries",
            "never-created".to_string(),
            NOT_FOUND,
            "Event bus never-created does not exist.".to_string(),
        ),
        (
            "the partner event bus form, which no bus here carries",
            "aws.partner/example.com/1234/stream".to_string(),
            NOT_FOUND,
            "Event bus aws.partner/example.com/1234/stream does not exist.".to_string(),
        ),
    ];

    for (case, name, code, expected) in rows {
        let (status, body) = call(&emulator, "DescribeEventBus", &json!({ "Name": name })).await;
        assert_eq!(
            status, 400,
            "case {case:?} name {name:?}: expected a refusal, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            code,
            "case {case:?} name {name:?}: expected {code}, got {body}"
        );
        let actual = message(&body);
        assert_eq!(
            actual, expected,
            "case {case:?} name {name:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// The exemption is real, not merely a different refusal: a partner event bus —
/// the one bus an account holds in the reserved namespace — is created by naming
/// the partner event source it matches, and is then read back and carries rules
/// through the very operations that refuse every other reserved name.
#[tokio::test]
async fn a_partner_event_bus_is_addressed_by_its_reserved_name() {
    let emulator = emulator();
    let source = "aws.partner/example.com/1234/stream";
    // The registration that makes this account the SaaS partner of the
    // namespace happens outside EventBridge, so it is written straight into the
    // store the handlers read.
    emulator
        .engine
        .state::<ComAmazonawsEventbridgeAwseventsState>()
        .lock()
        .register_partner_namespace("aws.partner/example.com");

    let (status, body) = call(
        &emulator,
        "CreatePartnerEventSource",
        &json!({ "Name": source, "Account": account() }),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreatePartnerEventSource answered {status} {body}"
    );

    let (status, body) = call(
        &emulator,
        "CreateEventBus",
        &json!({ "Name": source, "EventSourceName": source }),
    )
    .await;
    assert_eq!(status, 200, "CreateEventBus answered {status} {body}");
    assert_eq!(
        body["EventBusArn"],
        json!(event_bus_arn(source)),
        "the partner bus must report the ARN of its own name, got {body}"
    );

    // The read the issue was filed about: the same name, now a bus this account
    // holds, is reported rather than refused — by name and by ARN.
    for addressed in [source.to_string(), event_bus_arn(source)] {
        let (status, body) =
            call(&emulator, "DescribeEventBus", &json!({ "Name": addressed })).await;
        assert_eq!(
            status, 200,
            "case {addressed:?}: DescribeEventBus answered {status} {body}"
        );
        assert_eq!(
            body["Name"],
            json!(source),
            "case {addressed:?}: expected the partner bus name, got {body}"
        );
    }

    // And the rule operations reach it under that name too.
    let (status, body) = call(
        &emulator,
        "PutRule",
        &json!({
            "Name": "partner-rule",
            "EventBusName": source,
            "EventPattern": json!({ "source": [source] }).to_string(),
        }),
    )
    .await;
    assert_eq!(status, 200, "PutRule answered {status} {body}");
    let (status, body) = call(
        &emulator,
        "ListRules",
        &json!({ "EventBusName": source }),
    )
    .await;
    assert_eq!(status, 200, "ListRules answered {status} {body}");
    let listed: Vec<&str> = body["Rules"]
        .as_array()
        .map(|rules| {
            rules
                .iter()
                .filter_map(|rule| rule["Name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        listed,
        vec!["partner-rule"],
        "expected the rule written on the partner bus to be listed under its name, got {body}"
    );
}
