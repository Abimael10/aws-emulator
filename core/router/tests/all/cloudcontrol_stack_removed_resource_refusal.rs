// SPDX-License-Identifier: CC0-1.0

//! How Cloud Control API NAMES a resource the CloudFormation stack that owned it removed, in
//! the refusal of a later resource-level call.
//!
//! The RULE under test: **a resource-level Cloud Control operation refuses an absent resource
//! with the owning handler's own not-found verdict, and the resource named inside that verdict
//! is rendered as the HANDLER that owns the resource type names it — the identifier the request
//! carried for a type whose handler echoes the reference it was given, and the type's
//! primary-identifier object `{"/properties/<property>":"<value>"}` for a type whose handler
//! resolves that reference against the type's registry schema first — whichever resource-level
//! operation asked and whichever API created and removed the resource.** Around that diagnostic
//! the refusal is fixed text, `<TypeName> Handler returned status FAILED: <diagnostic>
//! (HandlerErrorCode: NotFound, RequestToken: <token>)`, whose variable parts are the type the
//! request named, the identifier it addressed and the token minted for the handler invocation
//! that failed.
//!
//! `GetResource$Identifier` is where the model states the two forms a primary identifier may
//! be given in ("Primary identifiers can be specified as a string or JSON"); which of the two
//! a handler echoes back inside its own diagnostic is that handler's business, so the
//! rendering is PER TYPE and a case about one type says nothing about another. The types driven
//! here are the two halves of that split: a queue, whose handler repeats the reference the
//! request carried, and a log group, whose handler names the resource as its schema's
//! identifier object.
//!
//! Three services have to agree for the rule to hold over this walk, which is why the suite
//! lives here rather than in any one crate: CloudFormation deploys the resources and later
//! removes them, the services that OWN those types hold them, and Cloud Control API reads them
//! and composes the refusals. Overlap, stated rather than hidden: the Cloud Control crate's own
//! `deleted_resource_refusal` suite holds the same rule for a resource Cloud Control itself or
//! the owning service's own wire removed, with those services mounted alone. What only this
//! suite reaches is a resource a STACK owned, over the production service set — the join a
//! client makes when it reconciles a stack it deleted against the identifiers it still holds.
//!
//! One stack declares every row, so the walk is one deployment and one teardown. The rows
//! differ along what the rule generalises over: the resource type, which service holds its
//! resources, the form of the identifier the owning service assigns or the declaration carries
//! (a url that service assigned, the suffix a FIFO queue's name requires, a plain name, a
//! path-shaped name) and which resource-level operation refuses.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// One spelling of each wire this suite speaks, and of the production mounting it speaks them
// to: the Cloud Control and CloudFormation calls are the ones a sibling suite already
// addresses those services with (both signed for the Region the stack belongs to), and the
// router is the whole emulator over one engine, exactly as the binary assembles it.
use crate::cloudformation_apigatewayv2_api_schema_properties::{cloud_control, cloudformation};
use crate::cloudformation_provisions_stack_resources::xml_text;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::router as production_router;
use crate::query_wire::outputs_of;

/// The registry type whose resources live in the Amazon SQS service's own store, and whose
/// identifier is the queue url that service assigns.
const QUEUE_TYPE: &str = "AWS::SQS::Queue";

/// The registry type whose resources live in the CloudWatch Logs service's own store, and
/// whose identifier is the group's own name.
const LOG_GROUP_TYPE: &str = "AWS::Logs::LogGroup";

/// The stack every row is declared in.
const STACK: &str = "cc-gone-stack";

/// The `StackStatus` a completed create reports, and the suffix every status of a stack
/// operation that is still running carries.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const IN_PROGRESS_SUFFIX: &str = "_IN_PROGRESS";

/// The `HandlerErrorCode` a handler that could not find the resource fails with, and the
/// status this service answers a `ResourceNotFoundException` with.
const NOT_FOUND_CODE: &str = "NotFound";
const REFUSAL_STATUS: u16 = 400;

/// The `OperationStatus` values a resource operation request reports once its work is over.
const STATUS_SUCCESS: &str = "SUCCESS";
const STATUS_FAILED: &str = "FAILED";
const STATUS_CANCEL_COMPLETE: &str = "CANCEL_COMPLETE";

/// How long a wait in this suite runs before the case fails.
///
/// Every wait here is for work ANOTHER service performs — the stack's deployment and teardown,
/// each a further request of this emulator with the span the clock owes it — so the budget is
/// far above anything that flow takes, including on a host running the whole workspace's tests
/// at once. No case waits for a span: each re-reads until what it is after is there or this
/// deadline passes.
const BUDGET: Duration = Duration::from_secs(240);

/// How often such a wait re-reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The prefix every pointer of a resource type's registry schema carries.
const PROPERTY_POINTER_PREFIX: &str = "/properties/";

/// How the handler that owns a resource type names the resource inside its own diagnostic.
enum Rendering {
    /// The reference the request carried, which is what a handler built on the generic
    /// resource-provider base class echoes.
    AsAddressed,
    /// The type's primary-identifier object, keyed on the pointer the type's registry schema
    /// holds in `primaryIdentifier` — named here by the property that pointer points at.
    AsSchemaObject(&'static str),
}

impl Rendering {
    /// The identifier slot of the diagnostic, for a resource addressed as `identifier`.
    fn render(&self, identifier: &str) -> String {
        match self {
            Self::AsAddressed => identifier.to_string(),
            Self::AsSchemaObject(property) => {
                let mut named = serde_json::Map::new();
                named.insert(
                    format!("{PROPERTY_POINTER_PREFIX}{property}"),
                    Value::String(identifier.to_string()),
                );
                Value::Object(named).to_string()
            }
        }
    }
}

/// One row: a resource the stack declares, and how the type's handler names it.
struct Row {
    case: &'static str,
    type_name: &'static str,
    /// The logical id the stack declares it under, which is also the key its identifier is
    /// published as.
    logical_id: &'static str,
    /// The `Properties` the declaration carries.
    properties: Value,
    rendering: Rendering,
}

/// The rows, differing along the type, along which service holds its resources and along the
/// form of the identifier that service assigns or the declaration carries.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a queue the stack named, addressed by the url its service assigned",
            type_name: QUEUE_TYPE,
            logical_id: "Queue",
            properties: json!({ "QueueName": "cc-gone-queue" }),
            rendering: Rendering::AsAddressed,
        },
        Row {
            case: "a FIFO queue, whose name carries the suffix its own service requires",
            type_name: QUEUE_TYPE,
            logical_id: "OrderedQueue",
            properties: json!({ "QueueName": "cc-gone-queue-ordered.fifo", "FifoQueue": true }),
            rendering: Rendering::AsAddressed,
        },
        Row {
            case: "a log group the stack named",
            type_name: LOG_GROUP_TYPE,
            logical_id: "Group",
            properties: json!({ "LogGroupName": "cc-gone-group", "RetentionInDays": 7 }),
            rendering: Rendering::AsSchemaObject("LogGroupName"),
        },
        Row {
            case: "a path-shaped log group name",
            type_name: LOG_GROUP_TYPE,
            logical_id: "PathGroup",
            properties: json!({ "LogGroupName": "/aws-emulator/cc-gone/group" }),
            rendering: Rendering::AsSchemaObject("LogGroupName"),
        },
    ]
}

/// The template the rows are deployed from: every row's resource under its own logical id,
/// each publishing the identifier the owning service assigned it as a stack output — `Ref` on
/// either type returns that type's primary identifier (the queue url, the log group name).
fn template(rows: &[Row]) -> String {
    let mut resources = serde_json::Map::new();
    let mut outputs = serde_json::Map::new();
    for row in rows {
        let mut declaration = serde_json::Map::new();
        declaration.insert("Type".to_string(), Value::String(row.type_name.to_string()));
        declaration.insert("Properties".to_string(), row.properties.clone());
        resources.insert(row.logical_id.to_string(), Value::Object(declaration));
        outputs.insert(
            row.logical_id.to_string(),
            json!({ "Value": { "Ref": row.logical_id } }),
        );
    }
    json!({ "Resources": Value::Object(resources), "Outputs": Value::Object(outputs) }).to_string()
}

/// One CloudFormation call over its awsQuery wire, required to be served.
async fn cfn_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = cloudformation(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with body \
         {body}"
    );
    body
}

/// A string member of an answered document, or `""` when absent — so a missing member fails an
/// assertion rather than panicking the harness.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// The SHAPE a refusal names itself by: the `__type` the awsJson1_0 envelope carries, without
/// the namespace it may be qualified with — so a case names the error shape the model declares
/// rather than accepting any discriminator that merely embeds that name.
fn error_shape(body: &Value) -> String {
    let discriminator = text(body, "__type");
    match discriminator.rsplit_once('#') {
        Some((_, shape)) => shape.to_string(),
        None => discriminator,
    }
}

/// The message a refusal carries, under either casing the envelope may use.
fn stated_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// Read the stack until it reports a status that is no longer in progress, and answer that
/// `DescribeStacks` body.
async fn settled_stack(router: &MultiServiceRouter, case: &str) -> String {
    let started = Instant::now();
    loop {
        let described = cfn_ok(router, case, "DescribeStacks", &[("StackName", STACK)]).await;
        let reported = xml_text(&described, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with(IN_PROGRESS_SUFFIX) {
            return described;
        }
        assert!(
            started.elapsed() < BUDGET,
            "case {case:?}: expected the stack to settle within {BUDGET:?}, it still reports \
             {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read the resource `identifier` names through Cloud Control API, answering `(status, body)`.
async fn get_resource(
    router: &MultiServiceRouter,
    type_name: &str,
    identifier: &str,
) -> (u16, Value) {
    cloud_control(
        router,
        "GetResource",
        json!({ "TypeName": type_name, "Identifier": identifier }),
    )
    .await
}

/// Wait until the resource `identifier` names no longer resolves: what the stack's own deletion
/// owes the service that held it.
async fn no_longer_resolves(
    router: &MultiServiceRouter,
    case: &str,
    type_name: &str,
    identifier: &str,
) {
    let started = Instant::now();
    loop {
        let (status, body) = get_resource(router, type_name, identifier).await;
        if status != 200 {
            return;
        }
        assert!(
            started.elapsed() < BUDGET,
            "case {case:?}: expected the deleted stack's resource to stop resolving within \
             {BUDGET:?}, the read still answers {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A patch that changes nothing: every case here is about the refusal, not about the patch.
fn inert_patch() -> String {
    json!([{ "op": "add", "path": "/Tags", "value": [] }]).to_string()
}

/// The fixed text a handler-wrapped refusal brackets the handler's verdict with.
const WRAPPER_OPENING: &str = " Handler returned status FAILED: ";
const WRAPPER_CODE: &str = " (HandlerErrorCode: ";
const WRAPPER_TOKEN: &str = ", RequestToken: ";

/// The three facts the refusal carries, taken apart at the fixed text that brackets them: the
/// owning handler's diagnostic, the `HandlerErrorCode` it failed with and the `RequestToken` of
/// the invocation that failed.
fn handler_failure(stated: &str, type_name: &str, case: &str) -> (String, String, String) {
    let opening = format!("{type_name}{WRAPPER_OPENING}");
    let body = stated
        .strip_prefix(&opening)
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a message opening {opening:?} and closing \")\", got \
                 {stated:?}"
            )
        });
    let (detail, verdict) = body.rsplit_once(WRAPPER_CODE).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the message to name the handler error code after \
             {WRAPPER_CODE:?}, got {stated:?}"
        )
    });
    let (handler_error_code, request_token) = verdict.split_once(WRAPPER_TOKEN).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the message to name the failing request after \
             {WRAPPER_TOKEN:?}, got {stated:?}"
        )
    });
    (
        detail.to_string(),
        handler_error_code.to_string(),
        request_token.to_string(),
    )
}

/// The UNIFORM not-found sentence — what a handler built on the generic resource-provider base
/// class states, naming nothing but the type the request named and the resource as that
/// handler renders it.
fn not_found_diagnostic(type_name: &str, rendered: &str) -> String {
    format!("Resource of type '{type_name}' with identifier '{rendered}' was not found.")
}

/// Read the request `token` names until it reports a terminal status, answering its
/// `ProgressEvent`.
async fn terminal(router: &MultiServiceRouter, case: &str, token: &str) -> Value {
    let started = Instant::now();
    loop {
        let (status, answered) = cloud_control(
            router,
            "GetResourceRequestStatus",
            json!({ "RequestToken": token }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetResourceRequestStatus, got {status} with \
             {answered}"
        );
        let event = answered["ProgressEvent"].clone();
        let reached = text(&event, "OperationStatus");
        if [STATUS_SUCCESS, STATUS_FAILED, STATUS_CANCEL_COMPLETE].contains(&reached.as_str()) {
            return event;
        }
        assert!(
            started.elapsed() < BUDGET,
            "case {case:?}: expected the request to reach a terminal status within {BUDGET:?}, \
             it is still {reached:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every resource-level refusal of the resource `identifier` named, checked against the
/// diagnostic the owning handler states for it.
///
/// The two SYNCHRONOUS operations answer it inline, inside the envelope that carries the
/// handler error code and the token of the invocation that failed; the asynchronous delete is
/// ADMITTED and reports the same verdict on the request record it minted, where the diagnostic
/// and the handler error code each have a member of their own.
async fn assert_every_refusal_names_the_resource(
    router: &MultiServiceRouter,
    row: &Row,
    identifier: &str,
) {
    let expected = not_found_diagnostic(row.type_name, &row.rendering.render(identifier));

    let answers = [
        (
            "GetResource",
            json!({ "TypeName": row.type_name, "Identifier": identifier }),
        ),
        (
            "UpdateResource",
            json!({
                "TypeName": row.type_name,
                "Identifier": identifier,
                "PatchDocument": inert_patch(),
            }),
        ),
    ];
    for (operation, payload) in answers {
        let case = format!("{} ({operation})", row.case);
        let (status, body) = cloud_control(router, operation, payload).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        let shape = error_shape(&body);
        assert_eq!(
            shape, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {shape:?} in {body}"
        );
        let stated = stated_message(&body);
        let (detail, handler_error_code, request_token) =
            handler_failure(&stated, row.type_name, &case);
        assert_eq!(
            detail, expected,
            "case {case:?}: expected the handler diagnostic {expected:?}, got {detail:?}"
        );
        assert_eq!(
            handler_error_code, NOT_FOUND_CODE,
            "case {case:?}: expected the {NOT_FOUND_CODE} handler error code, got \
             {handler_error_code:?}"
        );
        // `RequestToken`, length 1..128, pattern `^[-A-Za-z0-9+/=]+$`.
        assert!(
            !request_token.is_empty()
                && request_token.len() <= 128
                && request_token
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()
                        || "-+/=".contains(character)),
            "case {case:?}: expected a RequestToken of the modelled form, got {request_token:?}"
        );
    }

    let case = format!("{} (DeleteResource)", row.case);
    let (status, accepted) = cloud_control(
        router,
        "DeleteResource",
        json!({ "TypeName": row.type_name, "Identifier": identifier }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {accepted}"
    );
    let token = text(&accepted["ProgressEvent"], "RequestToken");
    assert!(
        !token.is_empty(),
        "case {case:?}: expected the accepted delete to name its request, got {accepted}"
    );
    let event = terminal(router, &case, &token).await;
    assert_eq!(
        text(&event, "OperationStatus"),
        STATUS_FAILED,
        "case {case:?}: the delete of an absent resource must fail, got {event}"
    );
    assert_eq!(
        text(&event, "ErrorCode"),
        NOT_FOUND_CODE,
        "case {case:?}: expected the {NOT_FOUND_CODE} handler error code, got {event}"
    );
    let reported = text(&event, "StatusMessage");
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the handler diagnostic {expected:?}, got {reported:?}"
    );
}

/// RULE: once the stack that owned them is deleted, every resource-level Cloud Control
/// operation on the identifier each resource answered to is refused with the owning handler's
/// not-found verdict, naming the resource as THAT handler renders it — the queue by the
/// identifier the request carried, the log group as its schema's identifier object, from one
/// removal, so neither form can be flipped globally without failing here. While the stack
/// still holds them every resource reads back under the identifier it published, so each
/// refusal is about a resource that was removed rather than one that never existed.
#[tokio::test]
async fn a_refusal_after_the_owning_stack_is_deleted_names_the_resource_as_its_handler_does() {
    let router = production_router();
    let rows = rows();
    let case = "the stack that declares every row";
    let template = template(&rows);

    cfn_ok(
        &router,
        case,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let described = settled_stack(&router, case).await;
    let reported = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: expected the stack to complete, got {reported:?} with body {described}"
    );

    // The identifier each resource answers to is the one the stack publishes for it, and while
    // the stack holds the resource Cloud Control API reads it back under exactly that.
    let published = outputs_of(&described);
    let mut identifiers = Vec::with_capacity(rows.len());
    for row in &rows {
        let case = row.case;
        let identifier = published.get(row.logical_id).cloned().unwrap_or_default();
        assert!(
            !identifier.is_empty(),
            "case {case:?}: expected the stack to publish the identifier its service assigned, \
             got {published:?}"
        );
        let (status, served) = get_resource(&router, row.type_name, &identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the stack's resource to read back, got {status} with body \
             {served}"
        );
        assert_eq!(
            text(&served["ResourceDescription"], "Identifier"),
            identifier,
            "case {case:?}: expected the read to name the identifier the stack published, got \
             {served}"
        );
        identifiers.push(identifier);
    }

    // One teardown takes every one of them away...
    cfn_ok(&router, case, "DeleteStack", &[("StackName", STACK)]).await;
    for (row, identifier) in rows.iter().zip(&identifiers) {
        no_longer_resolves(&router, row.case, row.type_name, identifier).await;
    }

    // ...and each is then refused in its own type's rendering.
    for (row, identifier) in rows.iter().zip(&identifiers) {
        assert_every_refusal_names_the_resource(&router, row, identifier).await;
    }
}
