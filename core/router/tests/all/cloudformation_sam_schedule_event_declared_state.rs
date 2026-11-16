// SPDX-License-Identifier: CC0-1.0

//! A SAM function whose event source declares the trigger DISABLED really gets an Amazon
//! EventBridge rule that is disabled.
//!
//! The rule under test: **the `AWS::Serverless-2016-10-31` transform carries an event
//! source's declared ENABLEMENT onto the `AWS::Events::Rule` it expands that event into —
//! a declaration that states `State` states the rule's own state, and one that states the
//! boolean `Enabled` states it as EventBridge's `ENABLED` / `DISABLED` — so `DescribeRule`
//! on the synthesized rule reports the declared state, with everything else about the rule
//! (its schedule expression, its event pattern) carried through unchanged.** Three crates
//! have to agree for that to hold (CloudFormation expands the macro and makes the calls,
//! EventBridge holds the rule, Lambda and IAM answer for the function and the role the
//! transform synthesizes beside it), so the suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: WHICH member states the enablement
//! (`Enabled`, `State`, neither), the value it states (disabled, enabled), the FORM that
//! value is written in (a JSON boolean, the text a template spells a boolean as, one of
//! EventBridge's own state names), which event type the rule comes from (`Schedule`,
//! `EventBridgeRule`, `CloudWatchEvent`) and so whether the rule is driven by a schedule
//! expression or by an event pattern, and which schedule expression form it carries (`rate`,
//! `cron`).
//!
//! Two rows are TWINS: the same schedule expression, one declared enabled and one declared
//! disabled. Only the state may differ between them, which is what the last assertion pins.
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked — except each rule's name, which is the physical id the stack
//! published for it and what EventBridge is then asked to resolve.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this test crate writes ONCE
// (`tests/all/query_wire.rs`), and the crate's one waiter for a stack's clock-owed
// transition: the wait is on the CONDITION, looked for again until it holds.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// The `StackStatus` the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stack every row is deployed in: one stack, so the rows are expanded by one macro run
/// and the functions they declare come up together.
const STACK: &str = "sam-schedule-declared-state";

/// The `RuleState` values EventBridge names a rule's state with (`PutRule$State`).
const ENABLED: &str = "ENABLED";
const DISABLED: &str = "DISABLED";

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way
/// (the credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Amazon EventBridge operation (`sigv4Name: events`, service shape `AWSEvents`,
/// `awsJson1_1`).
async fn events(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header("authorization", authorization("events"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The physical id the stack published for one of its logical resources — for a rule, the
/// name EventBridge holds it under.
async fn physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "DescribeStackResource",
        &[("StackName", stack), ("LogicalResourceId", logical_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResource for {logical_id}, got \
         {status} with body {body}"
    );
    opt_element(&body, "PhysicalResourceId").unwrap_or_else(|| {
        panic!("case {case:?}: no <PhysicalResourceId> for {logical_id} in {body}")
    })
}

/// The PROCESSED template — the macro's output, which is what CloudFormation plans and
/// deploys — as the document it is.
async fn processed_template(router: &MultiServiceRouter, stack: &str) -> Value {
    let (status, body) = cloudformation(router, "GetTemplate", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from GetTemplate, got {status} with body {body}"
    );
    // Read through the decoding reader: the serializer escapes the body's own quotes, and
    // what is asserted is the template document rather than its escaping.
    let text = elements(&body, "TemplateBody")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no <TemplateBody> in {body}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("expected a JSON processed template, got {error}: {text}"))
}

/// One row of the table: one `AWS::Serverless::Function` with one EventBridge-rule event,
/// and what the expansion and EventBridge must then report for the rule it became.
struct Row {
    case: &'static str,
    /// The function's logical id, and the name the event is declared under: the rule the
    /// transform adds carries the two concatenated as its own logical id.
    function: &'static str,
    event: &'static str,
    /// The event's `Type`, as the SAM resource reference spells it.
    event_type: &'static str,
    /// The event's `Properties`, as this row declares them.
    properties: Value,
    /// The `State` the PROCESSED template's generated rule must declare — `None` for a row
    /// that declares no enablement at all, which states nothing for the transform to carry
    /// and leaves the state to EventBridge's own default for an omitted `State`.
    expected_declared_state: Option<&'static str>,
    /// The `ScheduleExpression` `DescribeRule` must report, for a schedule-driven row.
    expected_schedule: Option<&'static str>,
    /// The `EventPattern` it must report, as the DOCUMENT it is: EventBridge holds the TEXT
    /// of one, whose key order and spacing are the serializer's choice.
    expected_pattern: Option<Value>,
    /// The `RuleState` `DescribeRule` must report.
    expected_state: &'static str,
}

/// The schedule expression the two twin rows share: only their declared state differs.
const TWIN_SCHEDULE: &str = "rate(7 days)";

/// The event pattern the pattern-driven rows match on.
fn pattern() -> Value {
    json!({ "source": ["aws.ec2"], "detail-type": ["EC2 Instance State-change Notification"] })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a Schedule event declaring Enabled false",
            function: "FnPaused",
            event: "Tick",
            event_type: "Schedule",
            properties: json!({ "Schedule": TWIN_SCHEDULE, "Enabled": false }),
            expected_declared_state: Some(DISABLED),
            expected_schedule: Some(TWIN_SCHEDULE),
            expected_pattern: None,
            expected_state: DISABLED,
        },
        Row {
            case: "a Schedule event declaring Enabled true",
            function: "FnRunning",
            event: "Tick",
            event_type: "Schedule",
            properties: json!({ "Schedule": TWIN_SCHEDULE, "Enabled": true }),
            expected_declared_state: Some(ENABLED),
            expected_schedule: Some(TWIN_SCHEDULE),
            expected_pattern: None,
            expected_state: ENABLED,
        },
        Row {
            case: "a Schedule event declaring State DISABLED",
            function: "FnStatedOff",
            event: "Nightly",
            event_type: "Schedule",
            properties: json!({ "Schedule": "cron(0 8 * * ? *)", "State": DISABLED }),
            expected_declared_state: Some(DISABLED),
            expected_schedule: Some("cron(0 8 * * ? *)"),
            expected_pattern: None,
            expected_state: DISABLED,
        },
        Row {
            case: "a Schedule event declaring State ENABLED",
            function: "FnStatedOn",
            event: "Nightly",
            event_type: "Schedule",
            properties: json!({ "Schedule": "cron(15 3 * * ? *)", "State": ENABLED }),
            expected_declared_state: Some(ENABLED),
            expected_schedule: Some("cron(15 3 * * ? *)"),
            expected_pattern: None,
            expected_state: ENABLED,
        },
        Row {
            case: "a Schedule event declaring neither Enabled nor State",
            function: "FnDefaulted",
            event: "Hourly",
            event_type: "Schedule",
            properties: json!({ "Schedule": "rate(1 hour)" }),
            expected_declared_state: None,
            expected_schedule: Some("rate(1 hour)"),
            expected_pattern: None,
            expected_state: ENABLED,
        },
        Row {
            case: "a Schedule event whose Enabled is the text a JSON template spells a \
                   boolean as",
            function: "FnTextuallyPaused",
            event: "Tick",
            event_type: "Schedule",
            properties: json!({ "Schedule": "rate(30 minutes)", "Enabled": "false" }),
            expected_declared_state: Some(DISABLED),
            expected_schedule: Some("rate(30 minutes)"),
            expected_pattern: None,
            expected_state: DISABLED,
        },
        Row {
            case: "an EventBridgeRule event declaring Enabled false",
            function: "FnPatternPaused",
            event: "Changes",
            event_type: "EventBridgeRule",
            properties: json!({ "Pattern": pattern(), "Enabled": false }),
            expected_declared_state: Some(DISABLED),
            expected_schedule: None,
            expected_pattern: Some(pattern()),
            expected_state: DISABLED,
        },
        Row {
            case: "a CloudWatchEvent event declaring State DISABLED",
            function: "FnLegacyPaused",
            event: "Changes",
            event_type: "CloudWatchEvent",
            properties: json!({ "Pattern": pattern(), "State": DISABLED }),
            expected_declared_state: Some(DISABLED),
            expected_schedule: None,
            expected_pattern: Some(pattern()),
            expected_state: DISABLED,
        },
    ]
}

/// The SAM template every row is declared in: one function per row, each with the one event
/// that row states.
fn sam_template(rows: &[Row]) -> String {
    let resources: serde_json::Map<String, Value> = rows
        .iter()
        .map(|row| {
            (
                row.function.to_string(),
                json!({
                    "Type": "AWS::Serverless::Function",
                    "Properties": {
                        "Handler": "index.handler",
                        "Runtime": "python3.12",
                        "InlineCode": "def handler(event, context):\n    return 1\n",
                        "Events": {
                            row.event: {
                                "Type": row.event_type,
                                "Properties": row.properties.clone(),
                            }
                        }
                    }
                }),
            )
        })
        .collect();
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// The `Properties` of one resource of a processed template.
fn declared_properties(case: &str, template: &Value, logical_id: &str) -> Value {
    template
        .get("Resources")
        .and_then(|resources| resources.get(logical_id))
        .and_then(|resource| resource.get("Properties"))
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the processed template to declare {logical_id}, got {template}")
        })
}

#[tokio::test]
async fn a_scheduled_function_declared_disabled_gets_a_disabled_rule() {
    let router = build_router();
    let rows = rows();
    let template = sam_template(&rows);

    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The role the transform synthesizes for each function is acknowledged with
            // CAPABILITY_IAM, and the macro itself with CAPABILITY_AUTO_EXPAND.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {body}"
    );

    let (settled, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "expected the stack to deploy the expanded template, got {settled:?} with body \
         {described}"
    );

    let processed = processed_template(&router, STACK).await;
    // (case, the schedule expression reported, the state reported), for the twin check.
    let mut reported: Vec<(&str, Option<String>, String)> = Vec::new();

    for row in &rows {
        let case = row.case;
        let logical_id = format!("{}{}", row.function, row.event);

        // What the macro expanded the event into: the rule declares the state the event
        // stated, and nothing else about it changed.
        let properties = declared_properties(case, &processed, &logical_id);
        assert_eq!(
            properties.get("State").and_then(Value::as_str),
            row.expected_declared_state,
            "case {case:?}: expected the generated rule to declare State {:?}, got {properties}",
            row.expected_declared_state
        );

        // And what EventBridge itself holds under the name the stack published for it.
        let rule_name = physical_id(&router, case, STACK, &logical_id).await;
        let (status, rule) = events(&router, "DescribeRule", &json!({ "Name": rule_name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeRule on {rule_name:?}, got {status} \
             with body {rule}"
        );
        assert_eq!(
            rule.get("Name").and_then(Value::as_str),
            Some(rule_name.as_str()),
            "case {case:?}: expected DescribeRule to report the rule's own name, got {rule}"
        );
        assert_eq!(
            rule.get("ScheduleExpression").and_then(Value::as_str),
            row.expected_schedule,
            "case {case:?}: expected ScheduleExpression {:?}, got {rule}",
            row.expected_schedule
        );
        let pattern: Option<Value> = rule
            .get("EventPattern")
            .and_then(Value::as_str)
            .map(|text| {
                serde_json::from_str(text).unwrap_or_else(|error| {
                    panic!("case {case:?}: expected an event pattern document, got {error}: {text}")
                })
            });
        assert_eq!(
            pattern, row.expected_pattern,
            "case {case:?}: expected EventPattern {:?}, got {rule}",
            row.expected_pattern
        );
        let state = rule
            .get("State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            state, row.expected_state,
            "case {case:?}: expected the rule in state {:?}, got {rule}",
            row.expected_state
        );

        reported.push((
            case,
            rule.get("ScheduleExpression")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            state,
        ));
    }

    // The twins: the same schedule expression declared enabled and declared disabled differ
    // in NOTHING but the state, so a client shipping a paused schedule gets a rule that does
    // not fire and the one beside it still does.
    let twins: Vec<&(&str, Option<String>, String)> = reported
        .iter()
        .filter(|(_, schedule, _)| schedule.as_deref() == Some(TWIN_SCHEDULE))
        .collect();
    assert_eq!(
        twins.len(),
        2,
        "case \"the twins\": expected two rules on {TWIN_SCHEDULE:?}, got {reported:?}"
    );
    assert_eq!(
        (twins[0].2.as_str(), twins[1].2.as_str()),
        (DISABLED, ENABLED),
        "case \"the twins\": expected the declared states to tell the two rules apart, got \
         {reported:?}"
    );
}
