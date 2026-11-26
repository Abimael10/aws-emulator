// SPDX-License-Identifier: CC0-1.0

//! THE SHAPE OF AN IDENTIFIER THIS CONTROL PLANE GENERATES, over the wire.
//!
//! RULE under test, stated for every id the service mints rather than for one operation:
//! an identifier AWS IoT generates and hands back in the response that creates or starts a
//! resource falls in the SHAPE CLASS that service mints that resource's id in — its width,
//! its character class and its casing — and never in an arbitrary opaque token of some
//! other width or casing, even where the modelled pattern would admit one. The classes are
//! two: a registry resource (a thing, a thing group, a thing type, a mitigation action) is
//! identified by a canonical lowercase hyphenated version-4 UUID, and a Device Defender
//! audit task by a hyphen-free lowercase hexadecimal token of fixed width (the form
//! `taskId` takes in AWS IoT Device Defender's own `DescribeAuditTask` /
//! `ListAuditTasks` examples). Each id is unique per resource and inside the modelled
//! bound of the member that carries it.
//!
//! The rows are the service's id-minting operations, so one of them answering an id of
//! another shape class fails here whichever operation it is. Every case drives the real
//! wire path (restJson1 over `MultiServiceRouter::dispatch`), so what is asserted is what
//! an SDK client would see.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite is signed for. No assertion depends on it: an id
/// is not region-scoped.
const REGION: &str = "ap-southeast-2";

/// The role a mitigation action and the account's audit configuration name, of more than
/// the modelled minimum length (`RoleArn`, len 20..2048).
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/device-defender-mitigation";

/// An audit check the account enables so an on-demand audit may target it: an entry of the
/// service's catalog of Device Defender audit checks
/// (`service_com_amazonaws_iot_awsiotservice::state::AUDIT_CHECK_NAMES`).
const LOGGING_DISABLED_CHECK: &str = "LOGGING_DISABLED_CHECK";

/// The shape class an identifier falls in.
#[derive(Debug, Clone, Copy)]
enum ShapeClass {
    /// A version-4 UUID in the canonical lowercase hyphenated form: 36 characters in five
    /// groups of lowercase hexadecimal, 8-4-4-4-12.
    CanonicalUuid,
    /// A hyphen-free lowercase hexadecimal token of exactly this many characters.
    LowercaseHex(usize),
}

/// One id-minting operation: the request that mints an id, the member that carries it, the
/// shape class it falls in, and the modelled maximum length of that member.
struct Row {
    case: &'static str,
    path: &'static str,
    body: Value,
    member: &'static str,
    class: ShapeClass,
    max_length: usize,
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

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
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
            .expect("the request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        let json = serde_json::from_str(&text).unwrap_or(Value::Null);
        (status, json)
    }

    /// Drive `case`'s operation and answer the id it minted, requiring a 200.
    async fn minted(&self, case: &str, path: &str, body: Option<&Value>, member: &str) -> String {
        let (status, json) = self.dispatch("POST", path, body).await;
        assert_eq!(
            status, 200,
            "case {case}: expected 200 from POST {path}, got {status} body {json}"
        );
        json[member]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no {member} in {json}"))
            .to_string()
    }

    /// Set the account's audit configuration up, enabling one check, so an on-demand audit
    /// may be started at all.
    async fn enable_audit_check(&self, case: &str) {
        let (status, json) = self
            .dispatch(
                "PATCH",
                "/audit/configuration",
                Some(&json!({
                    "roleArn": ROLE_ARN,
                    "auditCheckConfigurations": {
                        LOGGING_DISABLED_CHECK: { "enabled": true },
                    },
                })),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected UpdateAccountAuditConfiguration to answer 200, got {status} \
             body {json}"
        );
    }
}

/// Every character of a lowercase hexadecimal token.
fn is_lowercase_hex(character: char) -> bool {
    character.is_ascii_digit() || ('a'..='f').contains(&character)
}

/// Assert `id` is in `class`, and inside `max_length` — the modelled maximum of the member
/// that carries it.
///
/// Each class is written out here rather than read off the implementation's constants, so a
/// change to how an id is minted cannot leave a row green.
fn assert_shape_class(case: &str, id: &str, class: ShapeClass, max_length: usize) {
    match class {
        ShapeClass::CanonicalUuid => {
            assert_eq!(
                id.chars().count(),
                36,
                "case {case}: expected an id of 36 characters, got {id:?}"
            );
            let groups: Vec<&str> = id.split('-').collect();
            assert_eq!(
                groups
                    .iter()
                    .map(|group| group.chars().count())
                    .collect::<Vec<_>>(),
                vec![8, 4, 4, 4, 12],
                "case {case}: expected the 8-4-4-4-12 hyphenated groups, got {id:?}"
            );
            assert!(
                groups
                    .iter()
                    .all(|group| group.chars().all(is_lowercase_hex)),
                "case {case}: expected lowercase hexadecimal groups, got {id:?}"
            );
            let parsed = ::uuid::Uuid::parse_str(id)
                .unwrap_or_else(|error| panic!("case {case}: {id:?} is no UUID: {error}"));
            assert_eq!(
                parsed.get_version_num(),
                4,
                "case {case}: expected a version-4 UUID, got {id:?}"
            );
            assert_eq!(
                parsed.to_string(),
                id,
                "case {case}: expected the canonical rendering, got {id:?}"
            );
        }
        ShapeClass::LowercaseHex(width) => {
            assert_eq!(
                id.chars().count(),
                width,
                "case {case}: expected an id of {width} characters, got {id:?}"
            );
            assert!(
                id.chars().all(is_lowercase_hex),
                "case {case}: expected a lowercase hexadecimal id, got {id:?}"
            );
            assert!(
                !id.contains('-'),
                "case {case}: expected an id with no hyphen in it, got {id:?}"
            );
        }
    }
    // Inside the carrier member's modelled length, and in the character class every one of
    // those members admits (letters, digits and the hyphen).
    assert!(
        (1..=max_length).contains(&id.chars().count())
            && id
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-'),
        "case {case}: expected an id inside the modelled length 1..{max_length} and pattern, \
         got {id:?}"
    );
}

/// RULE: every identifier this control plane generates falls in the shape class the service
/// mints that resource's id in, whichever operation mints it.
#[tokio::test]
async fn every_generated_resource_identifier_is_in_its_shape_class() {
    let emulator = Emulator::new();
    emulator.enable_audit_check("the audit configuration").await;

    // One row per id-minting operation, each with its shape class and with the modelled
    // maximum length of the member that carries the id
    // (`smithy_query(query="operation",operation=<row>)`): AuditTaskId 1..40,
    // ThingGroupId 1..128, and ThingId / ThingTypeId / MitigationActionId bounded nowhere
    // in the model, read here against the widest bound the others carry.
    let rows = vec![
        Row {
            case: "the taskId StartOnDemandAuditTask mints",
            path: "/audit/tasks",
            body: json!({ "targetCheckNames": [LOGGING_DISABLED_CHECK] }),
            member: "taskId",
            class: ShapeClass::LowercaseHex(32),
            max_length: 40,
        },
        Row {
            case: "the thingId CreateThing mints",
            path: "/things/minted-id-thing",
            body: json!({}),
            member: "thingId",
            class: ShapeClass::CanonicalUuid,
            max_length: 128,
        },
        Row {
            case: "the thingGroupId CreateThingGroup mints",
            path: "/thing-groups/minted-id-group",
            body: json!({}),
            member: "thingGroupId",
            class: ShapeClass::CanonicalUuid,
            max_length: 128,
        },
        Row {
            case: "the thingTypeId CreateThingType mints",
            path: "/thing-types/minted-id-type",
            body: json!({}),
            member: "thingTypeId",
            class: ShapeClass::CanonicalUuid,
            max_length: 128,
        },
        Row {
            case: "the actionId CreateMitigationAction mints",
            path: "/mitigationactions/actions/minted-id-action",
            body: json!({
                "roleArn": ROLE_ARN,
                "actionParams": { "enableIoTLoggingParams": {
                    "roleArnForLogging": ROLE_ARN,
                    "logLevel": "WARN",
                } },
            }),
            member: "actionId",
            class: ShapeClass::CanonicalUuid,
            max_length: 128,
        },
    ];

    let mut minted: BTreeSet<String> = BTreeSet::new();
    for row in &rows {
        let id = emulator
            .minted(row.case, row.path, Some(&row.body), row.member)
            .await;
        assert_shape_class(row.case, &id, row.class, row.max_length);
        assert!(
            minted.insert(id.clone()),
            "case {}: expected an id no other resource holds, got the already-issued {id:?}",
            row.case
        );
    }
}

/// RULE, read along the other axis: starting the SAME operation repeatedly mints a fresh id
/// of that one shape class every time — the shape is the service's, the value is the run's.
#[tokio::test]
async fn a_repeated_start_mints_a_fresh_identifier_of_the_same_shape() {
    let emulator = Emulator::new();
    emulator.enable_audit_check("the audit configuration").await;

    let body = json!({ "targetCheckNames": [LOGGING_DISABLED_CHECK] });
    let mut minted: BTreeSet<String> = BTreeSet::new();
    let mut characters: BTreeSet<char> = BTreeSet::new();
    for attempt in 0..12 {
        let case = format!("audit attempt {attempt}");
        let task_id = emulator
            .minted(&case, "/audit/tasks", Some(&body), "taskId")
            .await;
        assert_shape_class(&case, &task_id, ShapeClass::LowercaseHex(32), 40);
        assert!(
            minted.insert(task_id.clone()),
            "case {case}: expected an id no earlier audit was started under, got the \
             already-issued {task_id:?}"
        );
        characters.extend(task_id.chars());

        // ROUND TRIP: the audit is readable back under exactly the id it answered, so the
        // reshaped id is the key the store holds it under.
        let (status, report) = emulator
            .dispatch("GET", &format!("/audit/tasks/{task_id}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected DescribeAuditTask to answer 200 for {task_id:?}, got {status} \
             body {report}"
        );
        assert_eq!(
            report["taskType"],
            json!("ON_DEMAND_AUDIT_TASK"),
            "case {case}: expected the audit started on demand, got {report}"
        );
    }

    // The CLASS is SPANNED, read over every character of every id above: a token of 32
    // characters that only ever drew on the digits (or only on a-f) would be a narrower
    // class than the one AWS mints in, so both halves of the hexadecimal alphabet have to
    // occur.
    for (class, present) in [
        ("a digit", char::is_ascii_digit as fn(&char) -> bool),
        (
            "a letter a-f",
            (|character: &char| ('a'..='f').contains(character)) as fn(&char) -> bool,
        ),
    ] {
        assert!(
            characters.iter().any(present),
            "case {class}: expected the minted ids to draw on it, got the characters \
             {characters:?}"
        );
    }
}
