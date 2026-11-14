// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Events::EventBus` really creates the bus in
//! Amazon EventBridge, so the rules, grants and rule targets of the SAME stack resolve it.
//!
//! The rule under test: **a resource type a stack declares is created in the service that
//! owns it, under the name the stack reports for that resource — so another resource of the
//! same stack that resolves its `Ref` or `Fn::GetAtt`, and any later client call, finds it
//! there.** Two crates have to agree for that to hold (CloudFormation makes the calls,
//! EventBridge serves them), so the suite lives here rather than in either one.
//!
//! The rows differ along what the rule generalises over: which intrinsic the dependent
//! resource resolves the bus with (`Ref`, `Fn::GetAtt … Name`, `Fn::GetAtt … Arn`), whether
//! the grant on the bus is declared as a whole policy `Statement` or as the members that
//! spell out one grant, whether the bus declares a `Description` and `Tags`, how many grants
//! one bus carries, whether the template is JSON or YAML, and which Region the stack is
//! deployed in.
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked — except each resource's physical id, which is what the stack
//! reports for the resource and what EventBridge is then asked to resolve.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::time::Instant;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use super::query_wire::{form_encode, opt_element, outputs_of, physical_ids_of};

/// How long a stack operation is given to leave `*_IN_PROGRESS`.
///
/// The wait below is on the CONDITION — it looks again until the stack has settled — so this
/// is only the point at which nothing settled it at all. Generous, so a loaded host cannot
/// decide the outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long between two looks at a stack that has not settled yet.
const SETTLE_INTERVAL: Duration = Duration::from_millis(50);

/// The account id every ARN this emulator mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// The IAM role a grant written as a whole statement admits. IAM is global, so the ARN
/// carries no Region.
const ADMITTED_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/a-publisher";

/// The account a grant written as `PutPermission`'s own members admits:
/// `PutPermission$Principal` is "the 12-digit Amazon Web Services account ID", whose
/// modelled pattern is `^(\d{12}|\*)$`.
const ADMITTED_ACCOUNT: &str = "210987654321";

/// The one action an event bus's resource policy grants
/// (`PutPermission$Action`: "the action that you are enabling the other account to perform").
const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
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
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
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
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one Amazon EventBridge operation (`sigv4Name: events`, service shape `AWSEvents`,
/// which speaks `awsJson1_1`).
async fn events(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header("authorization", authorization("events", region))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The error code an awsJson answer carries, as a client branches on it.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .map(|code| code.rsplit('#').next().unwrap_or(code).to_string())
        .unwrap_or_default()
}

/// The `DescribeStacks` answer for `stack_name` once its operation has left
/// `*_IN_PROGRESS`, or a panic when nothing settled it inside [`SETTLE_DEADLINE`].
async fn settled_stack(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack_name: &str,
) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body \
             {described}"
        );
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {stack_name} was still {reached:?} after {SETTLE_DEADLINE:?}, so \
             nothing settled it: {described}"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// The ARN EventBridge mints for the bus `name` in `region`.
fn event_bus_arn(region: &str, name: &str) -> String {
    format!("arn:aws:events:{region}:{ACCOUNT_ID}:event-bus/{name}")
}

/// `DescribeEventBus` for the bus `name` once EventBridge no longer holds it, or the last
/// answer read when nothing removed it inside [`SETTLE_DEADLINE`].
///
/// A stack's deletion walks its resources, so the bus is gone once that walk has reached it:
/// what is waited for is the CONDITION, looked at again until it holds, and the deadline is
/// only the point at which nothing performed the delete at all.
async fn bus_gone(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    name: &str,
) -> (u16, Value) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let answered = events(router, region, "DescribeEventBus", &json!({ "Name": name })).await;
        if answered.0 != 200 {
            return answered;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: EventBridge still held the bus {name:?} after {SETTLE_DEADLINE:?}, \
             so the stack's deletion never removed it: {:?}",
            answered.1
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// The statements one bus's resource policy carries, keyed by the `Sid` that identifies each
/// grant, as a client reads the document `DescribeEventBus` reports in `Policy`.
fn policy_statements(described: &Value) -> BTreeMap<String, Value> {
    let document: Value = described
        .get("Policy")
        .and_then(Value::as_str)
        .map(|text| serde_json::from_str(text).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);
    let statements = match document.get("Statement") {
        Some(Value::Array(items)) => items.clone(),
        Some(statement @ Value::Object(_)) => vec![statement.clone()],
        _ => Vec::new(),
    };
    statements
        .into_iter()
        .filter_map(|statement| {
            let sid = statement.get("Sid").and_then(Value::as_str)?.to_string();
            Some((sid, statement))
        })
        .collect()
}

/// `DescribeEventBus` for one bus, asserting the read succeeds.
async fn describe_event_bus(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    name: &str,
) -> Value {
    let (status, described) = events(router, region, "DescribeEventBus", &json!({ "Name": name }))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeEventBus on the name the stack \
         reported, got {status} with body {described}"
    );
    described
}

/// The principal a grant's statement must name, as the policy language writes one, paired
/// with the statement id that identifies the grant.
///
/// The principal is a function rather than a literal because a `serde_json` value is not a
/// constant; each row's grants are read through [`Grant`] so the table stays one table.
type Grant = (&'static str, fn() -> Value);

/// One row of the table: a template, and what the rule says the bus it declares — and the
/// grants and the rule bound to it — must then be in EventBridge.
struct Row {
    case: &'static str,
    /// The Region the stack is deployed in, and so the Region the bus lives in.
    region: &'static str,
    /// The stack's name, unique across this suite.
    stack_name: &'static str,
    /// The template body, in the form the row writes it (JSON or YAML).
    template: fn() -> String,
    /// The bus's logical id and the name the template declares for it.
    bus_logical_id: &'static str,
    declared_bus_name: &'static str,
    /// A SECOND bus the same stack declares, where the row declares one: the destination a
    /// rule target forwards to, which must resolve in EventBridge exactly as the first does.
    declared_destination_bus: Option<&'static str>,
    /// What `DescribeEventBus` must report for the bus's own description.
    expected_description: Option<&'static str>,
    /// The tag the declaration asks for, where it asks for one.
    expected_tags: &'static [(&'static str, &'static str)],
    /// The rule the stack binds to that bus, and the name it declares for it.
    declared_rule_name: &'static str,
    /// Each grant the stack installs on the bus: its statement id and the `Principal` the
    /// statement must name, as the policy language writes one.
    expected_grants: &'static [Grant],
}

/// The principal a grant written as a whole statement names: the role ARN, under `AWS`.
fn role_principal() -> Value {
    json!({ "AWS": ADMITTED_ROLE_ARN })
}

/// The principal `PutPermission`'s own members produce for a named account: the account id
/// as the request gave it, under `AWS`.
fn account_principal() -> Value {
    json!({ "AWS": ADMITTED_ACCOUNT })
}

const ROWS: &[Row] = &[
    Row {
        case: "a JSON bus with a Ref-bound rule and one statement grant",
        region: "us-west-2",
        stack_name: "events-bus-ref",
        template: || {
            json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Bus": {
                        "Type": "AWS::Events::EventBus",
                        "Properties": {
                            "Name": "events-bus-ref-bus",
                            "Description": "the stack's own bus",
                            "Tags": [{ "Key": "Owner", "Value": "platform" }]
                        }
                    },
                    "Destination": {
                        "Type": "AWS::Events::EventBus",
                        "Properties": { "Name": "events-bus-ref-destination" }
                    },
                    "Rule": {
                        "Type": "AWS::Events::Rule",
                        "Properties": {
                            "Name": "events-bus-ref-rule",
                            "EventBusName": { "Ref": "Bus" },
                            "EventPattern": { "source": ["app.orders"] },
                            "State": "ENABLED",
                            "Targets": [{
                                "Id": "Forward",
                                "Arn": { "Fn::GetAtt": ["Destination", "Arn"] },
                                "RoleArn": ADMITTED_ROLE_ARN
                            }]
                        }
                    },
                    "Grant": {
                        "Type": "AWS::Events::EventBusPolicy",
                        "Properties": {
                            "EventBusName": { "Ref": "Bus" },
                            "StatementId": "admit-the-publisher",
                            "Statement": {
                                "Effect": "Allow",
                                "Principal": { "AWS": ADMITTED_ROLE_ARN },
                                "Action": PUT_EVENTS_ACTION,
                                "Resource": { "Fn::GetAtt": ["Bus", "Arn"] }
                            }
                        }
                    }
                },
                "Outputs": {
                    "BusRef": { "Value": { "Ref": "Bus" } },
                    "BusName": { "Value": { "Fn::GetAtt": ["Bus", "Name"] } },
                    "BusArn": { "Value": { "Fn::GetAtt": ["Bus", "Arn"] } }
                }
            })
            .to_string()
        },
        bus_logical_id: "Bus",
        declared_bus_name: "events-bus-ref-bus",
        declared_destination_bus: Some("events-bus-ref-destination"),
        expected_description: Some("the stack's own bus"),
        expected_tags: &[("Owner", "platform")],
        declared_rule_name: "events-bus-ref-rule",
        expected_grants: &[("admit-the-publisher", role_principal)],
    },
    Row {
        case: "a YAML bus with a GetAtt-Name-bound rule and a member-form grant",
        region: "eu-west-1",
        stack_name: "events-bus-getatt",
        template: || {
            [
                "Resources:",
                "  Bus:",
                "    Type: AWS::Events::EventBus",
                "    Properties:",
                "      Name: events-bus-getatt-bus",
                "  Rule:",
                "    Type: AWS::Events::Rule",
                "    Properties:",
                "      Name: events-bus-getatt-rule",
                "      EventBusName: !GetAtt Bus.Name",
                "      EventPattern:",
                "        source:",
                "          - app.audit",
                "  Grant:",
                "    Type: AWS::Events::EventBusPolicy",
                "    Properties:",
                "      EventBusName: !GetAtt Bus.Name",
                "      StatementId: admit-the-account",
                "      Action: events:PutEvents",
                "      Principal: \"210987654321\"",
                "Outputs:",
                "  BusRef:",
                "    Value: !Ref Bus",
                "  BusName:",
                "    Value: !GetAtt Bus.Name",
                "  BusArn:",
                "    Value: !GetAtt Bus.Arn",
                "",
            ]
            .join("\n")
        },
        bus_logical_id: "Bus",
        declared_bus_name: "events-bus-getatt-bus",
        declared_destination_bus: None,
        expected_description: None,
        expected_tags: &[],
        declared_rule_name: "events-bus-getatt-rule",
        expected_grants: &[("admit-the-account", account_principal)],
    },
    Row {
        case: "a bus carrying two grants, each under its own statement id",
        region: "ap-northeast-1",
        stack_name: "events-bus-two-grants",
        template: || {
            json!({
                "Resources": {
                    "Bus": {
                        "Type": "AWS::Events::EventBus",
                        "Properties": { "Name": "events-bus-two-grants-bus" }
                    },
                    "Rule": {
                        "Type": "AWS::Events::Rule",
                        "Properties": {
                            "Name": "events-bus-two-grants-rule",
                            "EventBusName": { "Fn::GetAtt": ["Bus", "Arn"] },
                            "EventPattern": { "source": ["app.audit"] }
                        }
                    },
                    "RoleGrant": {
                        "Type": "AWS::Events::EventBusPolicy",
                        "Properties": {
                            "EventBusName": { "Ref": "Bus" },
                            "StatementId": "admit-the-publisher",
                            "Statement": {
                                "Effect": "Allow",
                                "Principal": { "AWS": ADMITTED_ROLE_ARN },
                                "Action": PUT_EVENTS_ACTION,
                                "Resource": { "Fn::GetAtt": ["Bus", "Arn"] }
                            }
                        }
                    },
                    "AccountGrant": {
                        "Type": "AWS::Events::EventBusPolicy",
                        "Properties": {
                            "EventBusName": { "Ref": "Bus" },
                            "StatementId": "admit-the-account",
                            "Action": PUT_EVENTS_ACTION,
                            "Principal": ADMITTED_ACCOUNT
                        }
                    }
                },
                "Outputs": {
                    "BusRef": { "Value": { "Ref": "Bus" } },
                    "BusName": { "Value": { "Fn::GetAtt": ["Bus", "Name"] } },
                    "BusArn": { "Value": { "Fn::GetAtt": ["Bus", "Arn"] } }
                }
            })
            .to_string()
        },
        bus_logical_id: "Bus",
        declared_bus_name: "events-bus-two-grants-bus",
        declared_destination_bus: None,
        expected_description: None,
        expected_tags: &[],
        declared_rule_name: "events-bus-two-grants-rule",
        expected_grants: &[
            ("admit-the-account", account_principal),
            ("admit-the-publisher", role_principal),
        ],
    },
];

/// Every row: the stack completes, the bus the template declared is one
/// `events DescribeEventBus` answers with under the name the stack reports, the rule and the
/// grants of the same stack are bound to that very bus, and deleting the stack deletes the
/// bus there.
#[tokio::test]
async fn a_stack_provisions_the_event_buses_its_template_declares() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let region = row.region;
        let stack_name = row.stack_name;
        let template = (row.template)();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let described = settled_stack(&router, region, case, stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );

        // The bus's physical id is the name the template declared, which is what `Ref`
        // returns and what every EventBridge call on the bus takes.
        let (status, resources) = cloudformation(
            &router,
            region,
            "DescribeStackResources",
            &[("StackName", stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             body {resources}"
        );
        let identifiers = physical_ids_of(&resources);
        let bus_name = identifiers
            .get(row.bus_logical_id)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            bus_name, row.declared_bus_name,
            "case {case:?}: expected the bus's physical id to be the declared name {:?}, got \
             {bus_name:?} in {resources}",
            row.declared_bus_name
        );

        // EventBridge itself resolves that bus, with the ARN it mints for it.
        let bus = describe_event_bus(&router, region, case, &bus_name).await;
        let expected_arn = event_bus_arn(region, &bus_name);
        assert_eq!(
            bus.get("Name").and_then(Value::as_str),
            Some(bus_name.as_str()),
            "case {case:?}: expected DescribeEventBus to report the bus's name, got {bus}"
        );
        assert_eq!(
            bus.get("Arn").and_then(Value::as_str),
            Some(expected_arn.as_str()),
            "case {case:?}: expected the bus ARN EventBridge mints ({expected_arn:?}), got {bus}"
        );
        assert_eq!(
            bus.get("Description").and_then(Value::as_str),
            row.expected_description,
            "case {case:?}: expected Description {:?}, got {bus}",
            row.expected_description
        );

        // The identifiers the stack publishes for the bus are the ones EventBridge holds.
        let outputs = outputs_of(&described);
        for (key, expected) in [
            ("BusRef", &bus_name),
            ("BusName", &bus_name),
            ("BusArn", &expected_arn),
        ] {
            assert_eq!(
                outputs.get(key),
                Some(expected),
                "case {case:?}: expected output {key:?} to be {expected:?}, got {:?}",
                outputs.get(key)
            );
        }

        // A second bus the same stack declares resolves there too, and is the destination the
        // rule's forwarding target names by its `Fn::GetAtt … Arn`.
        if let Some(destination) = row.declared_destination_bus {
            let bus = describe_event_bus(&router, region, case, destination).await;
            assert_eq!(
                bus.get("Arn").and_then(Value::as_str),
                Some(event_bus_arn(region, destination).as_str()),
                "case {case:?}: expected the destination bus's own ARN, got {bus}"
            );
        }

        // The rule the same stack declared is bound to that bus, not to the default one.
        let (status, rule) = events(
            &router,
            region,
            "DescribeRule",
            &json!({ "Name": row.declared_rule_name, "EventBusName": bus_name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeRule on the bus the stack created, \
             got {status} with body {rule}"
        );
        assert_eq!(
            rule.get("EventBusName").and_then(Value::as_str),
            Some(bus_name.as_str()),
            "case {case:?}: expected the rule bound to the stack's own bus, got {rule}"
        );

        // Each declared grant is one statement of that bus's resource policy, under the
        // statement id its declaration names, and the grants do not displace one another.
        let installed = policy_statements(&bus);
        let installed_ids: Vec<&str> = installed.keys().map(String::as_str).collect();
        let expected_ids: Vec<&str> = row.expected_grants.iter().map(|(sid, _)| *sid).collect();
        assert_eq!(
            installed_ids, expected_ids,
            "case {case:?}: expected the declared statement ids {expected_ids:?}, got {bus}"
        );
        for (sid, principal) in row.expected_grants {
            let statement = installed.get(*sid).cloned().unwrap_or(Value::Null);
            assert_eq!(
                statement.get("Principal"),
                Some(&principal()),
                "case {case:?}: expected the statement {sid:?} to name the principal {:?}, \
                 got {statement}",
                principal()
            );
            assert_eq!(
                statement.get("Action").and_then(Value::as_str),
                Some(PUT_EVENTS_ACTION),
                "case {case:?}: expected the statement {sid:?} to grant {PUT_EVENTS_ACTION}, \
                 got {statement}"
            );
        }

        // The tags the declaration asks for are on the bus, and the `aws:cloudformation:*`
        // tags are not: EventBridge refuses a tag key in that reserved namespace on every
        // call that takes tags, so CloudFormation leaves them off rather than failing the
        // stack over them.
        let (status, tagged) = events(
            &router,
            region,
            "ListTagsForResource",
            &json!({ "ResourceARN": expected_arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListTagsForResource, got {status} with body \
             {tagged}"
        );
        let held: Vec<(&str, &str)> = tagged
            .get("Tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(|tag| {
                        Some((
                            tag.get("Key").and_then(Value::as_str)?,
                            tag.get("Value").and_then(Value::as_str)?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            held, row.expected_tags,
            "case {case:?}: expected the declared tags {:?}, got {tagged}",
            row.expected_tags
        );

        // Deleting the stack deletes the bus in the service that holds it.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        let (status, gone) = bus_gone(&router, region, case, &bus_name).await;
        assert_eq!(
            (status, error_code(&gone).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: expected DescribeEventBus to answer 400 ResourceNotFoundException \
             once the stack deleted the bus, got {status} with body {gone}"
        );
    }
}

/// An update that changes what the template declares is applied to the bus the stack already
/// has, through EventBridge's own update path: the bus's description is replaced in place,
/// and a grant the template no longer declares is revoked while the other grants the bus
/// carries are left alone.
#[tokio::test]
async fn an_update_applies_the_changed_bus_declaration_in_place() {
    let router = build_router();
    let case = "a redeploy that re-describes the bus and drops one of its two grants";
    let region = "ca-central-1";
    let stack_name = "events-bus-update";
    let bus_name = "events-bus-update-bus";

    // (the bus's description, whether the second grant is still declared)
    let template = |description: &str, keep_second: bool| {
        let mut resources = json!({
            "Bus": {
                "Type": "AWS::Events::EventBus",
                "Properties": { "Name": bus_name, "Description": description }
            },
            "RoleGrant": {
                "Type": "AWS::Events::EventBusPolicy",
                "Properties": {
                    "EventBusName": { "Ref": "Bus" },
                    "StatementId": "admit-the-publisher",
                    "Statement": {
                        "Effect": "Allow",
                        "Principal": { "AWS": ADMITTED_ROLE_ARN },
                        "Action": PUT_EVENTS_ACTION,
                        "Resource": { "Fn::GetAtt": ["Bus", "Arn"] }
                    }
                }
            }
        });
        if keep_second {
            resources["AccountGrant"] = json!({
                "Type": "AWS::Events::EventBusPolicy",
                "Properties": {
                    "EventBusName": { "Ref": "Bus" },
                    "StatementId": "admit-the-account",
                    "Action": PUT_EVENTS_ACTION,
                    "Principal": ADMITTED_ACCOUNT
                }
            });
        }
        json!({ "Resources": resources }).to_string()
    };

    let created = template("before the update", true);
    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", &created)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = settled_stack(&router, region, case, stack_name).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
    );

    let bus = describe_event_bus(&router, region, case, bus_name).await;
    let before: Vec<String> = policy_statements(&bus).into_keys().collect();
    assert_eq!(
        before,
        vec![
            "admit-the-account".to_string(),
            "admit-the-publisher".to_string()
        ],
        "case {case:?}: expected both declared grants before the update, got {bus}"
    );

    let updated = template("after the update", false);
    let (status, body) = cloudformation(
        &router,
        region,
        "UpdateStack",
        &[("StackName", stack_name), ("TemplateBody", &updated)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    let described = settled_stack(&router, region, case, stack_name).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("UPDATE_COMPLETE"),
        "case {case:?}: expected the update to complete, got {described}"
    );

    let bus = describe_event_bus(&router, region, case, bus_name).await;
    assert_eq!(
        bus.get("Description").and_then(Value::as_str),
        Some("after the update"),
        "case {case:?}: expected the declared description to be in force, got {bus}"
    );
    let after: Vec<String> = policy_statements(&bus).into_keys().collect();
    assert_eq!(
        after,
        vec!["admit-the-publisher".to_string()],
        "case {case:?}: expected only the grant the update still declares, got {bus}"
    );
}

/// A bus the stack declares with a `Policy` of its own carries that whole document, and a
/// redeploy that drops the property leaves the bus with no policy at all — which is what
/// `PutPermission`'s document form and `RemovePermission`'s every-grant form do.
#[tokio::test]
async fn a_declared_bus_policy_is_installed_and_revoked_with_the_declaration() {
    let router = build_router();
    let case = "a bus declaring its own policy document, then dropping it";
    let region = "us-east-2";
    let stack_name = "events-bus-inline-policy";
    let bus_name = "events-bus-inline-policy-bus";
    let sid = "admit-the-publisher";

    let template = |with_policy: bool| {
        let mut properties = json!({ "Name": bus_name });
        if with_policy {
            properties["Policy"] = json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Sid": sid,
                    "Effect": "Allow",
                    "Principal": { "AWS": ADMITTED_ROLE_ARN },
                    "Action": PUT_EVENTS_ACTION,
                    "Resource": event_bus_arn(region, bus_name)
                }]
            });
        }
        json!({
            "Resources": {
                "Bus": { "Type": "AWS::Events::EventBus", "Properties": properties }
            }
        })
        .to_string()
    };

    let created = template(true);
    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", &created)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = settled_stack(&router, region, case, stack_name).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
    );

    let bus = describe_event_bus(&router, region, case, bus_name).await;
    let installed = policy_statements(&bus);
    assert_eq!(
        installed.keys().map(String::as_str).collect::<Vec<&str>>(),
        vec![sid],
        "case {case:?}: expected the declared policy's statement on the bus, got {bus}"
    );

    let (status, body) = cloudformation(
        &router,
        region,
        "UpdateStack",
        &[("StackName", stack_name), ("TemplateBody", &template(false))],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    let described = settled_stack(&router, region, case, stack_name).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("UPDATE_COMPLETE"),
        "case {case:?}: expected the update to complete, got {described}"
    );

    let bus = describe_event_bus(&router, region, case, bus_name).await;
    assert!(
        bus.get("Policy").is_none(),
        "case {case:?}: expected a bus whose declaration states no policy to carry none, got \
         {bus}"
    );
}
