// SPDX-License-Identifier: CC0-1.0

//! What an EventBridge rule hands its Lambda function target, and WHETHER it hands it over at
//! all — end to end over the real `MultiServiceRouter` with EventBridge and Lambda mounted on
//! one engine.
//!
//! Three rules are pinned here, each over a table of inputs:
//!
//! 1. **What the target receives is what the target's own input configuration selects**: the
//!    whole event envelope by default, the target's constant `Target$Input` instead of
//!    anything from the event at all ("Valid JSON text passed to the target. In this case,
//!    nothing from the event itself is passed to the target"), or the one part of the event
//!    `Target$InputPath` names ("the value of the JSONPath that you specify is passed to the
//!    target"), where a path naming no place of the event selects JSON's empty value. A
//!    `PutTargets` call carrying a target id the rule already holds REPLACES that target's
//!    whole definition, so the configuration the next delivery follows is the one written
//!    last.
//!
//! 2. **Whether the function is invoked is decided by the function's own resource-based
//!    policy**, and a statement scoped by `aws:SourceArn` admits the deliveries of the rule
//!    whose ARN it names and no other. A rule's ARN carries the bus it lives on for every bus
//!    but `default`, so a grant naming the default-bus form of a name does not admit the
//!    deliveries of a rule of that same name on a custom bus, and the reverse holds too.
//!
//! 3. **A rule whose `EventPattern` does not match the event hands nothing over**, which for
//!    an `exists` term on a member nested inside `detail` means the term holds exactly where
//!    that member carries a LEAF: "exists only works on leaf nodes. It does not work on
//!    intermediate nodes", and `null` is a leaf the event carries while an object and an empty
//!    array are not.
//!
//! The observation is made where the delivery lands: on the asynchronous invocation Lambda
//! itself holds for the function, carrying the payload EventBridge handed over — "Event —
//! Invoke the function asynchronously" (`Invoke$InvocationType`) is the call a rule makes — so
//! a handover EventBridge recorded but Lambda never received fails here, as does one that
//! arrived carrying the wrong document.
//!
//! `crate::events_lambda_target_resource_policy` is the suite for which policy SHAPES admit the
//! invocation of a default-bus rule's target, and `crate::events_lambda_target_runs_the_handler`
//! for the queued delivery then being attempted; this one is the suite for the payload's shape
//! and for the bus segment of the rule ARN a grant is scoped by.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{InvocationOutcome, NO_PERMISSIONS};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The ARN namespace EventBridge mints its rule and bus ARNs under.
const EVENTS_ARN_NAMESPACE: &str = "events";

/// The `source` and `detail-type` every event this suite publishes carries, and what every
/// rule it writes matches on.
const SOURCE: &str = "lambda.target.input";
const DETAIL_TYPE: &str = "Input Shape Probe";

/// The principal EventBridge hands a resource-policy-governed target over as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The IAM action an invocation of a function performs, which is what a grant for this service
/// has to name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The bus a rule that names no bus lives on, whose rules carry no bus segment in their ARN.
const DEFAULT_BUS: &str = "default";

/// A deployment package as `Code.ZipFile` carries it. Nothing here asserts what a handler
/// computed: what is read is the event Lambda was handed for the function.
const ZIP_FILE: &str = "UEsDBAoAAAAAAAAAAAA=";

/// A well-formed execution role ARN. No IAM store is mounted, so it is a value the create
/// carries rather than a record this suite relies on.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// How long this suite waits for a span another service's clock owes, before giving up on it.
/// Generous: nothing here tests how fast a host runs.
const WAIT: Duration = Duration::from_secs(30);

/// How often it looks while waiting.
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// EventBridge and Lambda over ONE shared engine, as `core/router/src/lib.rs` mounts them.
fn router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, engine)
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("authorization", authorization("events"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Lambda `restJson1` call, as its status and parsed body.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: Value) -> (u16, Value) {
    let payload = if body.is_null() {
        Bytes::new()
    } else {
        Bytes::from(body.to_string())
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// How many times this suite looks while waiting for a span it does not own.
fn looks() -> u128 {
    WAIT.as_millis() / LOOK_INTERVAL.as_millis()
}

/// Create `name` and wait until Lambda first reports it `Active` — "the function is ready to be
/// invoked" (`FunctionConfiguration$State`) — so a delivery withheld later is the policy's or
/// the pattern's refusal and never the lifecycle's. The span waited for is that service's own,
/// so nothing here sleeps a fixed span on its behalf. Answers the function's ARN.
async fn create_function(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": ZIP_FILE },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda CreateFunction, got {status} ({created})"
    );
    let arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a FunctionArn, got {created}"))
        .to_string();
    for _ in 0..looks() {
        let (_, reported) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            Value::Null,
        )
        .await;
        if reported.get("State").and_then(Value::as_str) == Some("Active") {
            return arn;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("case {case:?}: Lambda never reported {name} Active");
}

/// Add one statement to a function's resource-based policy through `AddPermission`, which is
/// how a client grants this service the invocation.
async fn add_permission(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    statement_id: &str,
    source_arn: Option<&str>,
) {
    let mut body = json!({
        "StatementId": statement_id,
        "Action": INVOKE_ACTION,
        "Principal": EVENTS_PRINCIPAL,
    });
    if let Some(source_arn) = source_arn {
        body["SourceArn"] = json!(source_arn);
    }
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        body,
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda AddPermission, got {status} ({granted})"
    );
}

/// The event payload of every asynchronous invocation Lambda holds for `function`, as the TEXT
/// EventBridge handed over.
fn handed_to(engine: &Arc<engine::InMemoryEngine>, function: &str) -> Vec<String> {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let store = state.lock();
    store
        .functions
        .get(function)
        .map(|record| {
            record
                .async_invocations
                .iter()
                .map(|entry| String::from_utf8_lossy(&entry.payload).to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Wait until Lambda holds at least `expected` asynchronous invocations for `function`, and
/// answer what it holds then. A delivery is made while `PutEvents` is still being served, so
/// this converges at once in the ordinary case; the budget is for a loaded host.
async fn delivered(
    engine: &Arc<engine::InMemoryEngine>,
    function: &str,
    expected: usize,
) -> Vec<String> {
    for _ in 0..looks() {
        let held = handed_to(engine, function);
        if held.len() >= expected {
            return held;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    handed_to(engine, function)
}

/// What EventBridge recorded for each handover of a routed event.
fn handovers(engine: &Arc<engine::InMemoryEngine>) -> Vec<InvocationOutcome> {
    let state = engine.state::<service_com_amazonaws_eventbridge_awsevents::state::ComAmazonawsEventbridgeAwseventsState>();
    let store = state.lock();
    store
        .invocations
        .iter()
        .map(|invocation| invocation.outcome.clone())
        .collect()
}

/// Wait until EventBridge has recorded at least `expected` handovers, and answer what it
/// recorded then.
async fn recorded(engine: &Arc<engine::InMemoryEngine>, expected: usize) -> Vec<InvocationOutcome> {
    for _ in 0..looks() {
        let held = handovers(engine);
        if held.len() >= expected {
            return held;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    handovers(engine)
}

/// The `detail` every event of the input-shape table carries: a scalar member, a number, an
/// array and a nested object, so a path can name each kind of place.
fn detail() -> Value {
    json!({
        "order": "A-1",
        "amount": 7,
        "items": ["x", "y", "z"],
        "nested": { "member": "deep" },
    })
}

/// Publish one event carrying `detail` onto `bus` and answer nothing: the delivery is made
/// before the call is answered.
async fn publish(router: &MultiServiceRouter, case: &str, bus: &str, detail: &Value) {
    let published = events(
        router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "EventBusName": bus,
                "Detail": detail.to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the event to be accepted, got {published}"
    );
}

/// The pattern every rule in this suite matches on, as the JSON text `PutRule` takes.
fn pattern() -> String {
    json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string()
}

/// What a row expects the function to be handed.
#[derive(Debug, Clone)]
enum Expectation {
    /// The whole event envelope, whose `detail` is the published document.
    WholeEnvelope,
    /// Exactly this document and nothing else.
    Exactly(Value),
}

/// Assert `payload` is the whole envelope of an event carrying `detail`.
fn assert_whole_envelope(case: &str, payload: &Value, detail: &Value, text: &str) {
    // The members a published event's envelope carries, which is what a pattern names and what
    // a target taking the whole event receives
    // (`smithy_query(query="doc",shape="TestEventPattern",member="Event")` lists the mandatory
    // ones: id, account, source, time, region, resources, detail-type).
    for member in ["version", "id", "account", "time", "region", "resources"] {
        assert!(
            payload.get(member).is_some(),
            "case {case:?}: expected the envelope member {member:?} in what the function was \
             handed, got {text}"
        );
    }
    assert_eq!(
        payload.get("source"),
        Some(&json!(SOURCE)),
        "case {case:?}: expected the envelope's source to be {SOURCE:?}, got {text}"
    );
    assert_eq!(
        payload.get("detail-type"),
        Some(&json!(DETAIL_TYPE)),
        "case {case:?}: expected the envelope's detail-type to be {DETAIL_TYPE:?}, got {text}"
    );
    assert_eq!(
        payload.get("detail"),
        Some(detail),
        "case {case:?}: expected the envelope to carry the published detail {detail}, got {text}"
    );
}

/// Assert that the ONE document `handed` holds is what `expected` says it should be.
fn assert_handed(case: &str, handed: &[String], expected: &Expectation, detail: &Value) {
    assert_eq!(
        handed.len(),
        1,
        "case {case:?}: expected exactly one delivery to the function, got {handed:?}"
    );
    let text = &handed[0];
    let payload: Value = serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("case {case:?}: expected a JSON document as the payload, got {text:?} ({error})")
    });
    match expected {
        Expectation::WholeEnvelope => assert_whole_envelope(case, &payload, detail, text),
        Expectation::Exactly(document) => assert_eq!(
            &payload, document,
            "case {case:?}: expected the function to be handed {document}, got {text}"
        ),
    }
}

/// One row of the input-shape table: what the target's input configuration says, and what the
/// function is handed because of it.
struct InputRow {
    case: &'static str,
    /// The members the target carries beside its `Id` and `Arn`.
    configuration: Value,
    expected: Expectation,
}

fn input_rows() -> Vec<InputRow> {
    vec![
        InputRow {
            case: "a target with no input configuration receives the whole envelope",
            configuration: json!({}),
            expected: Expectation::WholeEnvelope,
        },
        InputRow {
            case: "an InputPath naming the detail hands the function the detail alone",
            configuration: json!({ "InputPath": "$.detail" }),
            expected: Expectation::Exactly(detail()),
        },
        InputRow {
            case: "an InputPath naming a member nested in the detail hands that member over",
            configuration: json!({ "InputPath": "$.detail.nested.member" }),
            expected: Expectation::Exactly(json!("deep")),
        },
        InputRow {
            case: "an InputPath naming a number hands that number over",
            configuration: json!({ "InputPath": "$.detail.amount" }),
            expected: Expectation::Exactly(json!(7)),
        },
        InputRow {
            case: "an InputPath naming one element of an array hands that element over",
            configuration: json!({ "InputPath": "$.detail.items[1]" }),
            expected: Expectation::Exactly(json!("y")),
        },
        InputRow {
            case: "an InputPath naming a hyphenated envelope member hands that member over",
            configuration: json!({ "InputPath": "$.detail-type" }),
            expected: Expectation::Exactly(json!(DETAIL_TYPE)),
        },
        InputRow {
            // A path that selects no place of the event selects JSON's empty value, so what
            // travels is the text of that value.
            case: "an InputPath naming no place of the event hands the empty value over",
            configuration: json!({ "InputPath": "$.detail.nope" }),
            expected: Expectation::Exactly(Value::Null),
        },
        InputRow {
            // "In this case, nothing from the event itself is passed to the target".
            case: "a constant Input replaces the event with the target's own object",
            configuration: json!({ "Input": "{\"constant\":true,\"stage\":\"prod\"}" }),
            expected: Expectation::Exactly(json!({ "constant": true, "stage": "prod" })),
        },
        InputRow {
            case: "a constant Input that is a JSON string is handed over as that string",
            configuration: json!({ "Input": "\"just text\"" }),
            expected: Expectation::Exactly(json!("just text")),
        },
        InputRow {
            case: "a constant Input that is a JSON number is handed over as that number",
            configuration: json!({ "Input": "41" }),
            expected: Expectation::Exactly(json!(41)),
        },
    ]
}

/// Every row of the input-shape table, each on a router of its own.
#[tokio::test]
async fn a_function_target_is_handed_what_its_input_configuration_selects() {
    for (index, row) in input_rows().iter().enumerate() {
        let case = row.case;
        let (router, engine) = router();
        let function_name = format!("input-shape-{index}");
        let rule_name = format!("input-shape-rule-{index}");
        let function_arn = create_function(&router, case, &function_name).await;
        add_permission(&router, case, &function_name, "events-invoke", None).await;

        events(
            &router,
            case,
            "PutRule",
            &json!({ "Name": rule_name, "EventPattern": pattern(), "State": "ENABLED" }),
        )
        .await;
        let mut target = json!({ "Id": "fn", "Arn": function_arn });
        for (member, value) in row
            .configuration
            .as_object()
            .expect("each row's configuration is an object")
        {
            target[member] = value.clone();
        }
        let wired = events(
            &router,
            case,
            "PutTargets",
            &json!({ "Rule": rule_name, "Targets": [target] }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected the target to be accepted, got {wired}"
        );

        let detail = detail();
        publish(&router, case, DEFAULT_BUS, &detail).await;

        let outcomes = recorded(&engine, 1).await;
        assert_eq!(
            outcomes,
            vec![InvocationOutcome::Invoked],
            "case {case:?}: expected the handover to be invoked, got {outcomes:?}"
        );
        let handed = delivered(&engine, &function_name, 1).await;
        assert_handed(case, &handed, &row.expected, &detail);
    }
}

/// A `PutTargets` call naming a target id the rule already holds replaces that target's whole
/// definition, so the input configuration a delivery follows is the one written LAST — read
/// over three successive writes of ONE target id on ONE rule, each followed by its own event.
#[tokio::test]
async fn a_target_upsert_decides_what_later_deliveries_carry() {
    let case = "three successive writes of one target id";
    let (router, engine) = router();
    let function_name = "upsert-probe";
    let rule_name = "upsert-probe-rule";
    let function_arn = create_function(&router, case, function_name).await;
    add_permission(&router, case, function_name, "events-invoke", None).await;
    events(
        &router,
        case,
        "PutRule",
        &json!({ "Name": rule_name, "EventPattern": pattern(), "State": "ENABLED" }),
    )
    .await;

    let detail = detail();
    // (what this write of the target carries, what the delivery after it must hand over)
    let writes = [
        (
            json!({ "Id": "fn", "Arn": function_arn, "InputPath": "$.detail" }),
            Expectation::Exactly(detail.clone()),
        ),
        (
            json!({
                "Id": "fn",
                "Arn": function_arn,
                "Input": "{\"replaced\":\"by the upsert\"}",
            }),
            Expectation::Exactly(json!({ "replaced": "by the upsert" })),
        ),
        (
            json!({ "Id": "fn", "Arn": function_arn }),
            Expectation::WholeEnvelope,
        ),
    ];

    for (index, (target, expected)) in writes.iter().enumerate() {
        let case = format!("{case}: write {index} carrying {target}");
        let wired = events(
            &router,
            &case,
            "PutTargets",
            &json!({ "Rule": rule_name, "Targets": [target] }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected the target to be accepted, got {wired}"
        );
        // One target id written three times is ONE target of the rule, never three.
        let listed = events(
            &router,
            &case,
            "ListTargetsByRule",
            &json!({ "Rule": rule_name }),
        )
        .await;
        let targets = listed["Targets"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            targets.len(),
            1,
            "case {case:?}: expected the rule to hold one target, got {listed}"
        );

        publish(&router, &case, DEFAULT_BUS, &detail).await;
        let handed = delivered(&engine, function_name, index + 1).await;
        assert_eq!(
            handed.len(),
            index + 1,
            "case {case:?}: expected {} delivery(ies) by now, got {handed:?}",
            index + 1
        );
        assert_handed(&case, &handed[index..], expected, &detail);
    }
}

/// `arn:aws:events:<region>:<account>:rule/<bus-if-not-default>/<name>` — the ARN EventBridge
/// mints for a rule, where the bus segment is carried by every bus but `default`.
fn rule_arn(bus: &str, name: &str) -> String {
    let resource = if bus == DEFAULT_BUS {
        format!("rule/{name}")
    } else {
        format!("rule/{bus}/{name}")
    };
    engine::emulator_arn(EVENTS_ARN_NAMESPACE, REGION, &resource)
}

/// One row of the grant-scope table: which bus the rule lives on, which bus the ARN the grant
/// is scoped to belongs to, and whether the delivery is admitted.
struct GrantRow {
    case: &'static str,
    /// The bus the rule is written on.
    rule_bus: &'static str,
    /// The bus whose form of the rule's name the grant's `SourceArn` names — `None` for a
    /// grant that is not scoped at all.
    granted_bus: Option<&'static str>,
    invoked: bool,
}

/// The custom bus the rows below write a rule on, which is any bus that is not `default`.
const CUSTOM_BUS: &str = "input-shape-bus";

fn grant_rows() -> Vec<GrantRow> {
    vec![
        GrantRow {
            case: "a default-bus rule, granted its own default-bus ARN",
            rule_bus: DEFAULT_BUS,
            granted_bus: Some(DEFAULT_BUS),
            invoked: true,
        },
        GrantRow {
            case: "a custom-bus rule, granted only the default-bus ARN of that name",
            rule_bus: CUSTOM_BUS,
            granted_bus: Some(DEFAULT_BUS),
            invoked: false,
        },
        GrantRow {
            case: "a custom-bus rule, granted its own custom-bus ARN",
            rule_bus: CUSTOM_BUS,
            granted_bus: Some(CUSTOM_BUS),
            invoked: true,
        },
        GrantRow {
            case: "a default-bus rule, granted only the custom-bus ARN of that name",
            rule_bus: DEFAULT_BUS,
            granted_bus: Some(CUSTOM_BUS),
            invoked: false,
        },
        GrantRow {
            case: "a custom-bus rule, granted the invocation unscoped",
            rule_bus: CUSTOM_BUS,
            granted_bus: None,
            invoked: true,
        },
    ]
}

/// Every row of the grant-scope table, each on a router of its own: the rule ARN EventBridge
/// mints, the handover EventBridge recorded, and what Lambda holds for the function.
#[tokio::test]
async fn a_grant_scoped_by_source_arn_admits_only_the_named_rules_bus() {
    for (index, row) in grant_rows().iter().enumerate() {
        let case = row.case;
        let (router, engine) = router();
        let function_name = format!("bus-scope-{index}");
        // ONE name, written on whichever bus the row names, so the only thing that can tell
        // the admitted delivery from the refused one is the bus segment of the rule's ARN.
        let rule_name = format!("bus-scope-rule-{index}");
        let function_arn = create_function(&router, case, &function_name).await;

        if row.rule_bus != DEFAULT_BUS {
            events(
                &router,
                case,
                "CreateEventBus",
                &json!({ "Name": row.rule_bus }),
            )
            .await;
        }
        let rule = events(
            &router,
            case,
            "PutRule",
            &json!({
                "Name": rule_name,
                "EventBusName": row.rule_bus,
                "EventPattern": pattern(),
                "State": "ENABLED",
            }),
        )
        .await;
        let minted = rule["RuleArn"].as_str().unwrap_or_default();
        let expected_arn = rule_arn(row.rule_bus, &rule_name);
        assert_eq!(
            minted, expected_arn,
            "case {case:?}: expected the rule ARN {expected_arn}, got {minted:?}"
        );

        if let Some(granted_bus) = row.granted_bus {
            let scope = rule_arn(granted_bus, &rule_name);
            add_permission(&router, case, &function_name, "events-invoke", Some(&scope)).await;
        } else {
            add_permission(&router, case, &function_name, "events-invoke", None).await;
        }

        let wired = events(
            &router,
            case,
            "PutTargets",
            &json!({
                "Rule": rule_name,
                "EventBusName": row.rule_bus,
                "Targets": [{ "Id": "fn", "Arn": function_arn }],
            }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected the target to be accepted, got {wired}"
        );

        let detail = detail();
        publish(&router, case, row.rule_bus, &detail).await;

        // The rule matched either way, so a handover is recorded either way: what the row
        // decides is what that handover came to.
        let outcomes = recorded(&engine, 1).await;
        assert_eq!(
            outcomes.len(),
            1,
            "case {case:?}: expected one recorded handover, got {outcomes:?}"
        );
        let handed = delivered(&engine, &function_name, usize::from(row.invoked)).await;

        if row.invoked {
            assert_eq!(
                outcomes,
                vec![InvocationOutcome::Invoked],
                "case {case:?}: expected the delivery to be invoked, got {outcomes:?}"
            );
            assert_handed(case, &handed, &Expectation::WholeEnvelope, &detail);
            continue;
        }
        let InvocationOutcome::Failed { code, .. } = &outcomes[0] else {
            panic!("case {case:?}: expected a failed delivery, got {outcomes:?}");
        };
        assert_eq!(
            code, NO_PERMISSIONS,
            "case {case:?}: expected the condition {NO_PERMISSIONS}, got {code:?}"
        );
        assert!(
            handed.is_empty(),
            "case {case:?}: expected the function never to be invoked, got {handed:?}"
        );
    }
}

/// One row of the `exists` table: what the nested member of `detail` carries, and whether the
/// rule therefore matches.
struct ExistsRow {
    case: &'static str,
    /// What the member the pattern tests carries — `None` for a detail that omits it.
    member: Option<Value>,
    matches: bool,
}

/// The member nested inside `detail` whose presence the rule's pattern tests.
const TESTED_MEMBER: &str = "event-arn";

fn exists_rows() -> Vec<ExistsRow> {
    vec![
        ExistsRow {
            case: "the member carries a string",
            member: Some(json!("arn:aws:health:::event/SERVICE/EVENT/ID")),
            matches: true,
        },
        ExistsRow {
            case: "the detail omits the member",
            member: None,
            matches: false,
        },
        ExistsRow {
            // `null` is a leaf the event carries, so the member IS there.
            case: "the member carries null",
            member: Some(Value::Null),
            matches: true,
        },
        ExistsRow {
            case: "the member carries a number",
            member: Some(json!(7)),
            matches: true,
        },
        ExistsRow {
            // "exists only works on leaf nodes. It does not work on intermediate nodes".
            case: "the member carries an object",
            member: Some(json!({ "inner": "value" })),
            matches: false,
        },
        ExistsRow {
            case: "the member carries an empty array",
            member: Some(json!([])),
            matches: false,
        },
        ExistsRow {
            case: "the member carries an array holding one scalar",
            member: Some(json!(["one"])),
            matches: true,
        },
    ]
}

/// Every row of the `exists` table, published in ONE `PutEvents` request onto ONE rule: the
/// deliveries that arrive are exactly the rows the term holds for.
///
/// Publishing them together is what makes "nothing was delivered" a conclusion rather than a
/// race: a matched event's handover is made before the publishing call is answered, so once
/// the matching rows have arrived the non-matching ones have already been decided.
#[tokio::test]
async fn an_exists_term_on_a_nested_detail_member_decides_the_delivery() {
    let case = "an exists term on a member nested in the detail";
    let (router, engine) = router();
    let function_name = "exists-probe";
    let rule_name = "exists-probe-rule";
    let function_arn = create_function(&router, case, function_name).await;
    add_permission(&router, case, function_name, "events-invoke", None).await;

    events(
        &router,
        case,
        "PutRule",
        &json!({
            "Name": rule_name,
            "EventPattern": json!({
                "source": [SOURCE],
                "detail": { TESTED_MEMBER: [{ "exists": true }] },
            })
            .to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    events(
        &router,
        case,
        "PutTargets",
        &json!({ "Rule": rule_name, "Targets": [{ "Id": "fn", "Arn": function_arn }] }),
    )
    .await;

    let rows = exists_rows();
    let entries: Vec<Value> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            // Every entry carries a probe marker, so the delivered envelopes say WHICH rows
            // arrived; the tested member is present only where the row says it is.
            let mut detail = json!({ "probe": index });
            if let Some(member) = &row.member {
                detail[TESTED_MEMBER] = member.clone();
            }
            json!({
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": detail.to_string(),
            })
        })
        .collect();
    let expected: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.matches)
        .map(|(index, _)| index)
        .collect();

    let published = events(&router, case, "PutEvents", &json!({ "Entries": entries })).await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected every entry to be accepted, got {published}"
    );

    let handed = delivered(&engine, function_name, expected.len()).await;
    let mut arrived: Vec<usize> = handed
        .iter()
        .map(|text| {
            let payload: Value = serde_json::from_str(text).unwrap_or_else(|error| {
                panic!("case {case:?}: expected a JSON envelope, got {text:?} ({error})")
            });
            payload
                .pointer("/detail/probe")
                .and_then(Value::as_u64)
                .map(|probe| probe as usize)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a probe marker in the envelope, got {text}")
                })
        })
        .collect();
    arrived.sort_unstable();
    assert_eq!(
        arrived, expected,
        "case {case:?}: expected the rows {:?} to be delivered and no others, got {:?} — \
         rows: {:?}",
        expected
            .iter()
            .map(|index| rows[*index].case)
            .collect::<Vec<_>>(),
        arrived
            .iter()
            .map(|index| rows[*index].case)
            .collect::<Vec<_>>(),
        handed
    );
}
