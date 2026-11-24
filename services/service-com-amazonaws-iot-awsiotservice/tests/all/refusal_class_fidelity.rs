// SPDX-License-Identifier: CC0-1.0

//! WHICH ANSWER a control-plane request that resolves to nothing gets: the error CLASS, the
//! HTTP status and the sentence each family words it in, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is the status, the
//! `x-amzn-errortype` header a client resolves the error shape from, and the `message` an
//! SDK surfaces, not an internal function's return value.
//!
//! The rules under test:
//!
//! * A DELETE that states an END STATE is idempotent: `DeleteThingType` asks that this
//!   account hold no thing type under the name, so a name it already holds none under is
//!   answered with the operation's success status and empty body, having written nothing —
//!   not with a refusal, which no retry could get past. The procedure's own preconditions
//!   (deprecate, wait, detach) are asked only of a type that IS there to delete, and the
//!   READERS of a type still answer the registry family's 404.
//! * The DEVICE DEFENDER configuration families word a miss by naming the INPUT MEMBER the
//!   request carried the resource's name at — `Resource <member>=<name> was not found`, with
//!   no account qualifier and no terminating period — so `DescribeScheduledAudit`
//!   (`scheduledAuditName`) and `UpdateMitigationAction` (`findingActionName`) state one
//!   sentence with the member each resolves by.
//! * The V2 LOGGING family's not-configured refusal is ONE fixed sentence, naming the call
//!   that configures logging and stating that no logging levels are set; it interpolates
//!   nothing from the request, and it stops being answered once the options are set.
//! * A refusal's `message` is the SERVICE's own sentence. A client library's rendering of an
//!   error (`An error occurred (<Code>) when calling the <Operation> operation: <message>`)
//!   is built by that library around this member, so the member never carries it:
//!   `DescribeFleetMetric`'s 404 states the fleet-metric family's account-qualified sentence
//!   alone.
//!
//! Each table spans what its rule generalises over — the form and length of the identifier
//! within the modelled constraint, HOW a name came to be absent (never created, or removed by
//! an earlier call, or removed twice), the operation and the member it resolves by, and the
//! operations of the V2 logging family — rather than one recorded request.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    service_state, AUDIT_CHECK_NAMES, DEPRECATION_DELETION_WAIT,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id this emulator serves (`engine::emulator_account_id`), which the registry
/// families qualify a missing name with.
const ACCOUNT_ID: &str = "000000000000";

/// The region every request in this suite is signed for. None of these answers is regional.
const REGION: &str = "us-east-2";

/// The wire error shape of a resource this service does not hold.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The wire error shape of a read of logging that was never configured.
const NOT_CONFIGURED: &str = "NotConfiguredException";

/// A role ARN the account's V2 logging options may name. Nothing asserts on the text: the
/// member is an opaque `AwsArn` in the model, and it is here only so the account HAS logging
/// options.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/iot-logs";

/// A role ARN the account's audit configuration may name as the one Device Defender audits
/// run under. Nothing asserts on the text; it is here only so the account HAS an audit
/// configuration a scheduled audit can target a check of.
const AUDIT_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/device-defender-audit";

/// AWS's sentence for a request that needs V2 logging options the account never set, written
/// out here so the wording is pinned by this suite and not only by the constant under test.
const NOT_CONFIGURED_MESSAGE: &str =
    "SetV2LoggingOptions was not previously called. No logging levels have been set.";

/// How a client LIBRARY renders a refusal around the service's own `message`. The opening of
/// that rendering, which a wire `message` must never carry: a client that found it there
/// would print it twice.
const CLIENT_RENDERING_OPENING: &str = "An error occurred (";

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

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The `x-amzn-errortype` header, verbatim — including any namespace a plane qualifies
    /// the shape name with.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` member a modelled error carries.
    fn message(&self) -> Option<String> {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// Whether the answer carries no members at all — the empty body a success with no
    /// modelled output reports, in either form the wire takes it (no bytes, or `{}`).
    fn is_empty_body(&self) -> bool {
        match &self.body {
            Value::Null => true,
            Value::Object(members) => members.is_empty(),
            _ => false,
        }
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached the
/// way the production router reaches it. The engine is kept beside the router so a case can
/// age a stored deprecation instead of waiting out the service's published wait.
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

    /// Register a thing type, deprecate it and age the deprecation past the published wait,
    /// so the delete of it can go through without any case waiting on the clock.
    async fn deletable_thing_type(&self, case: &str, thing_type_name: &str) {
        let created = self
            .call(
                "POST",
                &format!("/thing-types/{thing_type_name}"),
                Some(&json!({})),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateThingType must succeed, got {created:?}"
        );
        let deprecated = self
            .call(
                "POST",
                &format!("/thing-types/{thing_type_name}/deprecate"),
                Some(&json!({})),
            )
            .await;
        assert_eq!(
            deprecated.status, 200,
            "case {case:?}: DeprecateThingType must succeed, got {deprecated:?}"
        );
        self.age_deprecation(thing_type_name, DEPRECATION_DELETION_WAIT);
    }

    /// Move the instant a stored thing type was DEPRECATED back by `age`, so a case can get
    /// past the post-deprecation wait without waiting for it. Nothing a request can reach
    /// does this; it stands in for minutes of clock.
    fn age_deprecation(&self, thing_type_name: &str, age: Duration) {
        let state = service_state(&*self.engine).expect("the IoT service store");
        let mut store = state.lock();
        let thing_type = store
            .thing_types
            .get_mut(thing_type_name)
            .unwrap_or_else(|| panic!("the store holds the thing type {thing_type_name:?}"));
        let deprecated_at = thing_type
            .deprecation_date
            .unwrap_or_else(|| panic!("the thing type {thing_type_name:?} carries a deprecation"));
        thing_type.deprecation_date = Some(
            deprecated_at
                .checked_sub(age)
                .expect("an instant that far back is representable"),
        );
    }

    async fn delete_thing_type(&self, thing_type_name: &str) -> Reply {
        self.call("DELETE", &format!("/thing-types/{thing_type_name}"), None)
            .await
    }

    async fn describe_thing_type(&self, thing_type_name: &str) -> Reply {
        self.call("GET", &format!("/thing-types/{thing_type_name}"), None)
            .await
    }
}

/// HOW a thing type came to be absent when the delete under test runs.
#[derive(Debug, Clone, Copy)]
enum Absence {
    /// Nobody ever registered the name.
    NeverCreated,
    /// A type was registered and removed by an earlier delete.
    DeletedOnce,
    /// A type was registered, removed, and the removal asked for again — so the second
    /// repeat is answered the same way as the first.
    DeletedTwice,
}

/// RULE: `DeleteThingType` states the end state — this account holds no thing type under the
/// name — so a name that is already absent is answered with the empty 200 a removal answers
/// and nothing is written.
#[tokio::test]
async fn deleting_a_thing_type_the_registry_holds_none_of_succeeds() {
    // The rows differ in the form and the length of the name within `ThingTypeName`'s
    // modelled 1..128 `^[a-zA-Z0-9:_-]+$`, and in HOW the name came to be absent.
    let rows: Vec<(&str, String, Absence)> = vec![
        ("a name nobody created", "no-such-type".to_string(), Absence::NeverCreated),
        ("a single-character name", "z".to_string(), Absence::NeverCreated),
        (
            "a name at the modelled maximum length",
            "m".repeat(128),
            Absence::NeverCreated,
        ),
        (
            "a name carrying the colon and underscore the pattern admits",
            "Fleet:Sensor_Type-9".to_string(),
            Absence::NeverCreated,
        ),
        (
            "a name an earlier delete removed",
            "removed-once".to_string(),
            Absence::DeletedOnce,
        ),
        (
            "a name two earlier deletes addressed",
            "removed-twice".to_string(),
            Absence::DeletedTwice,
        ),
    ];

    for (case, thing_type_name, absence) in rows {
        let emulator = Emulator::new();
        // One type the account DOES hold, so every row also shows the no-op wrote nothing.
        emulator
            .deletable_thing_type(case, "kept-type")
            .await;

        let repeats = match absence {
            Absence::NeverCreated => 0,
            Absence::DeletedOnce => 1,
            Absence::DeletedTwice => 2,
        };
        if repeats > 0 {
            emulator.deletable_thing_type(case, &thing_type_name).await;
            for repeat in 0..repeats {
                let removed = emulator.delete_thing_type(&thing_type_name).await;
                assert_eq!(
                    removed.status, 200,
                    "case {case:?}: delete {repeat} of a deletable type must succeed, got \
                     {removed:?}"
                );
            }
        }

        let answered = emulator.delete_thing_type(&thing_type_name).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: deleting a name the registry holds no type under must answer the \
             success status, got {answered:?}"
        );
        assert_eq!(
            answered.error_type, None,
            "case {case:?}: a successful delete names no error shape, got {answered:?}"
        );
        assert!(
            answered.is_empty_body(),
            "case {case:?}: the delete reports no members, got {answered:?}"
        );

        // Nothing was written: the absent name is still absent to its reader, and the type
        // the account does hold survived.
        let still_absent = emulator.describe_thing_type(&thing_type_name).await;
        assert_eq!(
            still_absent.status, 404,
            "case {case:?}: the delete must not register the name, got {still_absent:?}"
        );
        let kept = emulator.describe_thing_type("kept-type").await;
        assert_eq!(
            kept.status, 200,
            "case {case:?}: the delete of an absent name must leave the registry alone, got \
             {kept:?}"
        );
    }
}

/// RULE: the READERS of a thing type still answer the registry family's account-qualified
/// 404 — only the delete treats an absent name as the state it asks for.
#[tokio::test]
async fn reading_a_thing_type_the_registry_holds_none_of_is_still_a_not_found() {
    let emulator = Emulator::new();

    for (case, thing_type_name) in [
        ("a name nobody created", "no-such-type".to_string()),
        ("a name at the modelled maximum length", "m".repeat(128)),
    ] {
        // The delete says the end state holds; the read of the same name does not.
        let deleted = emulator.delete_thing_type(&thing_type_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the delete of an absent name succeeds, got {deleted:?}"
        );

        let read = emulator.describe_thing_type(&thing_type_name).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a read of an absent thing type is refused 404, got {read:?}"
        );
        assert_eq!(
            read.error_type.as_deref(),
            Some(NOT_FOUND),
            "case {case:?}: expected ResourceNotFoundException, got {read:?}"
        );
        assert_eq!(
            read.message(),
            Some(format!("Thing Type {ACCOUNT_ID}:{thing_type_name} not found.")),
            "case {case:?}: expected the registry family's sentence, got {read:?}"
        );
    }
}

/// One Device Defender configuration operation that resolves a resource by a named input
/// member, and a name the account holds none under.
#[derive(Debug)]
struct MemberMissCase {
    /// What this row is, for the assertion messages.
    case: String,
    method: &'static str,
    path: String,
    /// A body for the method that carries one.
    body: Option<Value>,
    /// The input member the operation resolves the resource by, as the sentence names it.
    member: &'static str,
    /// The name the request carried at that member.
    name: String,
}

/// RULE: a Device Defender configuration family states a miss by naming the input member the
/// request carried the resource's name at: `Resource <member>=<name> was not found`, with no
/// account qualifier and no terminating period.
#[tokio::test]
async fn a_device_defender_configuration_miss_names_the_member_it_resolves_by() {
    let emulator = Emulator::new();
    let mut rows = Vec::new();
    // Both operations' names are `^[a-zA-Z0-9_-]+$` within 1..128, so the rows differ in
    // length, case and separator across that one class.
    for name in [
        "a".to_string(),
        "missing-configuration".to_string(),
        "Missing_Name-42".to_string(),
        "n".repeat(128),
    ] {
        rows.push(MemberMissCase {
            case: format!("DescribeScheduledAudit on {name:?}"),
            method: "GET",
            path: format!("/audit/scheduledaudits/{name}"),
            body: None,
            // `smithy_query(query="operation",operation="DescribeScheduledAudit")`:
            // `scheduledAuditName` is the `@httpLabel` this operation resolves by.
            member: "scheduledAuditName",
            name,
        });
    }
    for name in [
        "b".to_string(),
        "missing-action".to_string(),
        "Missing_Action-7".to_string(),
        "m".repeat(128),
    ] {
        rows.push(MemberMissCase {
            case: format!("UpdateMitigationAction on {name:?}"),
            method: "PATCH",
            path: format!("/mitigationactions/actions/{name}"),
            body: Some(json!({})),
            // The member the update resolves an action by, as its sentence names it.
            member: "findingActionName",
            name,
        });
    }

    for row in rows {
        let MemberMissCase {
            case,
            method,
            path,
            body,
            member,
            name,
        } = row;
        let refused = emulator.call(method, &path, body.as_ref()).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: a configuration miss is refused 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some(NOT_FOUND),
            "case {case:?}: expected ResourceNotFoundException, got {refused:?}"
        );
        let expected = format!("Resource {member}={name} was not found");
        assert_eq!(
            refused.message(),
            Some(expected.clone()),
            "case {case:?}: expected {expected:?}, got {refused:?}"
        );
    }
}

/// RULE: a scheduled audit the account DOES hold is read back, so the sentence above is about
/// the lookup and not about the operation.
#[tokio::test]
async fn a_scheduled_audit_the_account_holds_is_read_back() {
    let emulator = Emulator::new();
    // "Checks must be enabled for your account" before a schedule may target one, so the
    // account's audit configuration is written first.
    let check_name = AUDIT_CHECK_NAMES
        .first()
        .expect("the catalog of audit checks is not empty");
    let configured = emulator
        .call(
            "PATCH",
            "/audit/configuration",
            Some(&json!({
                "roleArn": AUDIT_ROLE_ARN,
                "auditCheckConfigurations": { *check_name: { "enabled": true } },
            })),
        )
        .await;
    assert_eq!(
        configured.status, 200,
        "UpdateAccountAuditConfiguration must succeed, got {configured:?}"
    );

    let created = emulator
        .call(
            "POST",
            "/audit/scheduledaudits/nightly-audit",
            Some(&json!({
                "frequency": "DAILY",
                "targetCheckNames": [*check_name],
            })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "CreateScheduledAudit must succeed, got {created:?}"
    );

    let read = emulator
        .call("GET", "/audit/scheduledaudits/nightly-audit", None)
        .await;
    assert_eq!(read.status, 200, "the created schedule reads back, got {read:?}");
    assert_eq!(
        read.body.get("frequency").and_then(Value::as_str),
        Some("DAILY"),
        "the read reports the created frequency, got {read:?}"
    );
    assert_eq!(
        read.body.get("targetCheckNames"),
        Some(&json!([*check_name])),
        "the read reports the targeted check, got {read:?}"
    );
}

/// RULE: the V2 logging family's not-configured refusal is ONE fixed sentence, interpolating
/// nothing from the request, and it stops being answered once the options are set.
#[tokio::test]
async fn the_v2_logging_not_configured_refusal_is_one_fixed_sentence() {
    let emulator = Emulator::new();

    // The rows are the operations of the family that need the account's options, over
    // requests that differ in their query string and body — none of which the sentence
    // mentions.
    let rows: Vec<(&str, &str, &str, Option<Value>)> = vec![
        ("ListV2LoggingLevels, no filter", "GET", "/v2LoggingLevel", None),
        (
            "ListV2LoggingLevels, filtered by target type",
            "GET",
            "/v2LoggingLevel?targetType=THING_GROUP",
            None,
        ),
        (
            "ListV2LoggingLevels, one row per page",
            "GET",
            "/v2LoggingLevel?maxResults=1",
            None,
        ),
        ("GetV2LoggingOptions", "GET", "/v2LoggingOptions", None),
        (
            "GetV2LoggingOptions, verbose",
            "GET",
            "/v2LoggingOptions?verbose=true",
            None,
        ),
        (
            "SetV2LoggingLevel on the account default",
            "POST",
            "/v2LoggingLevel",
            Some(json!({
                "logTarget": { "targetType": "DEFAULT" },
                "logLevel": "WARN",
            })),
        ),
    ];

    for (case, method, path, body) in &rows {
        let refused = emulator.call(method, path, body.as_ref()).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: an account with no logging options is refused 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some(NOT_CONFIGURED),
            "case {case:?}: expected NotConfiguredException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            Some(NOT_CONFIGURED_MESSAGE.to_string()),
            "case {case:?}: expected {NOT_CONFIGURED_MESSAGE:?}, got {refused:?}"
        );
    }

    // Once the options are set, the same requests are served: the refusal was about the
    // account's state, not about the operations.
    let configured = emulator
        .call(
            "POST",
            "/v2LoggingOptions",
            Some(&json!({ "roleArn": ROLE_ARN, "defaultLogLevel": "WARN" })),
        )
        .await;
    assert_eq!(
        configured.status, 200,
        "SetV2LoggingOptions must succeed, got {configured:?}"
    );
    for (case, method, path, body) in &rows {
        let served = emulator.call(method, path, body.as_ref()).await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: a configured account is served, got {served:?}"
        );
    }
}

/// RULE: a refusal's `message` is the service's OWN sentence — the fleet-metric family's
/// account-qualified one here — and never a client library's rendering of an error around it.
#[tokio::test]
async fn a_fleet_metric_miss_states_the_services_own_sentence() {
    let emulator = Emulator::new();

    // `FleetMetricName` is `^[a-zA-Z0-9_\-\.]+$` within 1..128, so the rows differ in
    // length, case and the separators that pattern admits.
    for (case, metric_name) in [
        ("a single-character name", "f".to_string()),
        ("a name nobody created", "missing-metric".to_string()),
        ("a name carrying a dot and an underscore", "Fleet.Metric_9".to_string()),
        ("a name at the modelled maximum length", "n".repeat(128)),
    ] {
        let refused = emulator
            .call("GET", &format!("/fleet-metric/{metric_name}"), None)
            .await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: a read of an absent fleet metric is refused 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some(NOT_FOUND),
            "case {case:?}: expected ResourceNotFoundException, got {refused:?}"
        );
        let expected = format!("FleetMetric {ACCOUNT_ID}:{metric_name} not found.");
        assert_eq!(
            refused.message(),
            Some(expected.clone()),
            "case {case:?}: expected {expected:?}, got {refused:?}"
        );
        let message = refused.message().unwrap_or_default();
        assert!(
            !message.contains(CLIENT_RENDERING_OPENING),
            "case {case:?}: the wire message carries no client-library rendering, got \
             {message:?}"
        );
    }
}
