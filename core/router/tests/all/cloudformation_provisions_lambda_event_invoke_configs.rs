// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Lambda::EventInvokeConfig` really puts that
//! asynchronous-invocation configuration on the function qualifier it names, in AWS Lambda
//! itself — so the destination a template wires up receives the invocation records of the
//! events the function fails, and a configuration Lambda refuses fails the stack.
//!
//! The rules under test:
//!
//! 1. **A resource of a type this emulator provisions is created in the service that OWNS
//!    it, through that service's own public API.** So the configuration a stack declares is
//!    one `lambda get-function-event-invoke-config` reports on the qualifier the declaration
//!    names, carrying exactly the settings the template asked for, under the physical id
//!    that function and qualifier are joined into — the type's own composite identifier —
//!    and a stack deletion takes the configuration away again.
//! 2. **Whatever the owning service answers short of success fails the resource and the
//!    stack.** Lambda delivers an invocation record ACTING AS the function's execution role,
//!    so it refuses a configuration whose role the policies in force on it do not let make
//!    the write that delivers a record to the named destination with
//!    `InvalidParameterValueException` — and such a template is rolled back instead of
//!    deploying clean and then silently dropping every failure record.
//! 3. **The configuration a stack put is the one Lambda's asynchronous invocation path
//!    reads.** An event whose handler fails every attempt it is allowed delivers its
//!    invocation record to the `DestinationConfig$OnFailure` destination, and one whose
//!    handler returns delivers it to the `DestinationConfig$OnSuccess` destination.
//!
//! Four crates have to agree for that to hold (CloudFormation plans the stack and makes the
//! call, Lambda serves it, judges the role and runs the asynchronous invocation, IAM answers
//! for the role's policies, and Amazon SQS holds the destinations), so the suite lives here
//! rather than in any one crate.
//!
//! The rows of the first table differ along what rule 1 generalises over: WHICH destination
//! slots the declaration states, the settings it states beside them (each at a published
//! bound of its own member's range and one inside it), and the KIND of qualifier the
//! configuration belongs to — the unpublished version and an alias of a published one. The
//! rows of the second table differ along what the role is granted and over the KIND of
//! destination the write is judged for. The rows of the third differ along the outcome the
//! event reached.
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element};
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for — the Region `wait_for_stack_status` polls in
/// — and so the Region the stack's resources belong to.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The `StackStatus` values the rules are stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// Where `PutFunctionEventInvokeConfig`, `GetFunctionEventInvokeConfig` and
/// `DeleteFunctionEventInvokeConfig` are bound: `/2019-09-25/functions/{FunctionName}
/// /event-invoke-config`, with the qualifier an `@httpQuery`
/// (`smithy_query(query="operation",operation="GetFunctionEventInvokeConfig")`).
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// Where `Invoke` is bound (`/2015-03-31/functions/{FunctionName}/invocations`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The request header `Invoke$InvocationType` is bound to, and the value that QUEUES the
/// event instead of waiting for the run.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
/// See [`INVOCATION_TYPE_HEADER`].
const EVENT_TYPE: &str = "Event";

/// The qualifier of the function's unpublished version, which `Qualifier`'s own pattern
/// admits (`^\$(LATEST(\.PUBLISHED)?)|[a-zA-Z0-9-_$]+$`).
const UNPUBLISHED: &str = "$LATEST";

/// The alias one row declares and configures, so the pair the resource is identified by is
/// exercised on a qualifier that is not the unpublished version.
const ALIAS: &str = "live";

/// How long a wait for work the CLOCK owes goes on — a stack settling, an event's delivery
/// attempt and the record that follows it.
///
/// A ceiling on PATIENCE, never a span anything here counts on: every wait below looks again
/// until the thing it waits for is there or this deadline passes, so a slow or loaded host is
/// waited for and only something that never arrives at all fails a row.
const WAIT_DEADLINE: Duration = Duration::from_secs(120);
/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler every function below runs: it RAISES when its event asks it to and returns
/// otherwise, so which outcome a completion reached is the handler's own doing.
const HANDLER_SOURCE: &str =
    "def handler(event, context):\n    if event.get(\"fail\"):\n        raise \
     RuntimeError(\"the handler refused the event\")\n    return {\"handled\": event}\n";

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
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
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// Call one awsQuery operation (CloudFormation, IAM) over its own wire.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "2010-05-15",
        action,
        members,
    )
    .await
}

/// One IAM call. IAM is global, so the request is signed for this suite's Region only
/// because that is what routes it.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    query(
        router,
        "iam",
        "iam.amazonaws.com",
        "2010-05-08",
        action,
        members,
    )
    .await
}

/// One awsJson1_0 RPC call to Amazon SQS.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One AWS Lambda call over its restJson1 wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
) -> (u16, Value) {
    let payload = match body {
        Some(document) => serde_json::to_vec(document).expect("the request body serializes"),
        None => Vec::new(),
    };
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::from(payload))
                .expect("a well-formed request"),
        )
        .await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// The URI that addresses the asynchronous-invocation configuration of one qualifier.
fn config_uri(function: &str, qualifier: &str) -> String {
    format!(
        "{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config?Qualifier={}",
        form_encode(qualifier)
    )
}

/// The ARN a queue of `name` carries in this suite's Region.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
}

/// The ARN a function of `name` carries.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// The ARN a role of `name` carries. IAM is global, so the ARN carries no Region.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// A trust policy that lets AWS Lambda assume the role, which is what an execution role has
/// to carry for CloudFormation to hand it to Lambda.
fn lambda_trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole"
        }]
    })
    .to_string()
}

/// Create one queue and answer its URL and its ARN.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (status, created) = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the queue {name:?} to be created, got {status} with body \
         {created}"
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    (url, queue_arn(name))
}

/// Create one execution role in IAM, granting `granted` on `resources` where a row asks for
/// a grant at all, and answer its ARN.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    granted: &[&str],
    resources: &[String],
) -> String {
    let (status, body) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &lambda_trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the execution role to be created in IAM, got {status} with \
         body {body}"
    );
    if !granted.is_empty() {
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": granted,
                "Resource": resources,
            }]
        })
        .to_string();
        let (status, body) = iam(
            router,
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "destination-writes"),
                ("PolicyDocument", &document),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the inline policy to be written, got {status} with body \
             {body}"
        );
    }
    role_arn(name)
}

/// The template a row deploys: the function, the asynchronous-invocation configuration the
/// row declares on it, and — for a row configuring an ALIAS — the published version that
/// alias invokes and the alias itself.
///
/// The configuration names the function by `Ref`, which "returns the function name" for an
/// `AWS::Lambda::Function`, and that is the identifier `FunctionName` takes.
fn template(function: &str, role: &str, qualifier: &str, config: &Value) -> String {
    let mut properties = json!({
        "FunctionName": { "Ref": "Fn" },
        "Qualifier": qualifier,
    });
    for (member, value) in config.as_object().expect("the row states a document") {
        properties[member] = value.clone();
    }
    let mut resources = json!({
        "Fn": {
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": function,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": role,
                "Code": { "ZipFile": HANDLER_SOURCE }
            }
        },
        "FnEventInvokeConfig": {
            "Type": "AWS::Lambda::EventInvokeConfig",
            "Properties": properties
        }
    });
    if qualifier == ALIAS {
        resources["FnVersion"] = json!({
            "Type": "AWS::Lambda::Version",
            "Properties": { "FunctionName": { "Ref": "Fn" } }
        });
        resources["FnAlias"] = json!({
            "Type": "AWS::Lambda::Alias",
            "Properties": {
                "FunctionName": { "Ref": "Fn" },
                "Name": ALIAS,
                "FunctionVersion": { "Fn::GetAtt": ["FnVersion", "Version"] }
            }
        });
        // The configuration belongs to the alias, which has to EXIST before Lambda can be
        // asked to configure it: "DependsOn … specify that the creation of a specific
        // resource follows another".
        resources["FnEventInvokeConfig"]["DependsOn"] = json!(["FnAlias"]);
    }
    json!({ "Resources": resources }).to_string()
}

/// The physical id the stack published for one of its logical resources.
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
        "case {case:?}: expected HTTP 200 from DescribeStackResource, got {status} with body \
         {body}"
    );
    opt_element(&body, "PhysicalResourceId")
        .unwrap_or_else(|| panic!("case {case:?}: no PhysicalResourceId in {body}"))
}

/// Keep reading the configuration of `function`/`qualifier` until Lambda answers `wanted`,
/// and answer the last `(status, body)` it answered.
///
/// The wait is on that CONDITION, looking again until it holds or [`WAIT_DEADLINE`] passes.
async fn config_until(
    router: &MultiServiceRouter,
    function: &str,
    qualifier: &str,
    wanted: u16,
) -> (u16, Value) {
    let uri = config_uri(function, qualifier);
    let giving_up_at = Instant::now() + WAIT_DEADLINE;
    loop {
        let answered = lambda(router, "GET", &uri, None, &[]).await;
        if answered.0 == wanted || Instant::now() >= giving_up_at {
            return answered;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every message on the queue `url` addresses, read off it until there are at least `wanted`
/// of them or [`WAIT_DEADLINE`] passes.
async fn awaited(router: &MultiServiceRouter, case: &str, url: &str, wanted: usize) -> Vec<Value> {
    let giving_up_at = Instant::now() + WAIT_DEADLINE;
    let mut held: Vec<Value> = Vec::new();
    loop {
        let (status, received) = sqs(
            router,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ReceiveMessage, got {status} with body \
             {received}"
        );
        if let Some(messages) = received["Messages"].as_array() {
            held.extend(messages.iter().cloned());
        }
        if held.len() >= wanted {
            return held;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {wanted} message(s) on {url} within {WAIT_DEADLINE:?}, got \
             {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

// ---------------------------------------------------------------------------
// Rule 1: the declared configuration is the one Lambda holds
// ---------------------------------------------------------------------------

/// One row of the configuration table: what the declaration states, and the qualifier it
/// states it on.
struct ConfigRow {
    case: &'static str,
    /// The stack, the function and the queues the row owns.
    slug: &'static str,
    /// The qualifier the configuration belongs to.
    qualifier: &'static str,
    /// Whether the declaration states the on-success slot, and the on-failure slot.
    slots: (bool, bool),
    /// The `MaximumRetryAttempts` the declaration states, where it states one.
    retries: Option<i64>,
    /// The `MaximumEventAgeInSeconds` it states, where it states one.
    event_age: Option<i64>,
}

/// The rows differ along which destination slots the declaration states, the settings it
/// stated beside them — each published bound of `MaximumRetryAttempts` (`@range(min: 0, max:
/// 2)`) and of `MaximumEventAgeInSeconds` (`@range(min: 60, max: 21600)`), and a value inside
/// each range — and the KIND of qualifier the configuration belongs to.
const CONFIG_ROWS: &[ConfigRow] = &[
    ConfigRow {
        case: "an on-failure destination and no retries on the unpublished version",
        slug: "eic-failure",
        qualifier: UNPUBLISHED,
        slots: (false, true),
        retries: Some(0),
        event_age: None,
    },
    ConfigRow {
        case: "both destination slots, with the largest retry count and event age the \
               members admit",
        slug: "eic-both",
        qualifier: UNPUBLISHED,
        slots: (true, true),
        retries: Some(2),
        event_age: Some(21_600),
    },
    ConfigRow {
        case: "an on-success destination with the smallest event age the member admits",
        slug: "eic-success",
        qualifier: UNPUBLISHED,
        slots: (true, false),
        retries: Some(1),
        event_age: Some(60),
    },
    ConfigRow {
        case: "a configuration stating nothing but its destination",
        slug: "eic-bare",
        qualifier: UNPUBLISHED,
        slots: (false, true),
        retries: None,
        event_age: None,
    },
    ConfigRow {
        case: "a configuration on an alias of a published version",
        slug: "eic-alias",
        qualifier: ALIAS,
        slots: (false, true),
        retries: Some(0),
        event_age: Some(900),
    },
];

/// The `DestinationConfig` and settings one row declares, given the ARNs of its two queues.
fn declared_config(row: &ConfigRow, on_success: &str, on_failure: &str) -> Value {
    let mut destinations = json!({});
    if row.slots.0 {
        destinations["OnSuccess"] = json!({ "Destination": on_success });
    }
    if row.slots.1 {
        destinations["OnFailure"] = json!({ "Destination": on_failure });
    }
    let mut declared = json!({ "DestinationConfig": destinations });
    if let Some(retries) = row.retries {
        declared["MaximumRetryAttempts"] = json!(retries);
    }
    if let Some(age) = row.event_age {
        declared["MaximumEventAgeInSeconds"] = json!(age);
    }
    declared
}

#[tokio::test]
async fn a_declared_event_invoke_config_is_the_one_lambda_holds_for_the_qualifier() {
    for row in CONFIG_ROWS {
        let router = build_router();
        let case = row.case;
        let function = format!("{}-fn", row.slug);
        let stack = format!("{}-stack", row.slug);
        let (_, on_success) = given_queue(&router, case, &format!("{}-success", row.slug)).await;
        let (_, on_failure) = given_queue(&router, case, &format!("{}-failure", row.slug)).await;
        let role = given_role(
            &router,
            case,
            &format!("{}-exec", row.slug),
            &["sqs:SendMessage"],
            &[on_success.clone(), on_failure.clone()],
        )
        .await;
        let declared = declared_config(row, &on_success, &on_failure);
        let body = template(&function, &role, row.qualifier, &declared);

        let (status, answered) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", &stack), ("TemplateBody", &body)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
        );
        let (reached, described) = wait_for_stack_status(&router, &stack, CREATE_COMPLETE).await;
        assert_eq!(
            reached, CREATE_COMPLETE,
            "case {case:?}: expected the stack to deploy, got {described}"
        );

        // Rule 1: Lambda itself reports the configuration the template declared, on the
        // qualifier it declared it for.
        let (status, held) = lambda(
            &router,
            "GET",
            &config_uri(&function, row.qualifier),
            None,
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected AWS Lambda to hold the configuration the stack declared, \
             got {status} with body {held}"
        );
        assert_eq!(
            held["FunctionArn"].as_str(),
            Some(format!("{}:{}", function_arn(&function), row.qualifier).as_str()),
            "case {case:?}: expected the configuration on the qualified function ARN, got {held}"
        );
        assert_eq!(
            held["MaximumRetryAttempts"].as_i64(),
            row.retries,
            "case {case:?}: expected MaximumRetryAttempts {:?}, got {held}",
            row.retries
        );
        assert_eq!(
            held["MaximumEventAgeInSeconds"].as_i64(),
            row.event_age,
            "case {case:?}: expected MaximumEventAgeInSeconds {:?}, got {held}",
            row.event_age
        );
        for (slot, stated, destination) in [
            ("OnSuccess", row.slots.0, &on_success),
            ("OnFailure", row.slots.1, &on_failure),
        ] {
            let expected = if stated {
                Some(destination.as_str())
            } else {
                None
            };
            let reported = held["DestinationConfig"][slot]["Destination"].as_str();
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the {slot} destination {expected:?}, got {held}"
            );
        }

        // ...under the physical id that function and qualifier are joined into, which is what
        // a `Ref` to the resource resolves to: every call about the configuration takes both.
        let published = physical_id(&router, case, &stack, "FnEventInvokeConfig").await;
        assert_eq!(
            published,
            format!("{function}:{}", row.qualifier),
            "case {case:?}: expected the stack to publish the pair the configuration is \
             identified by, got {published:?}"
        );

        // And deleting the stack takes the configuration away again.
        let (status, answered) =
            cloudformation(&router, "DeleteStack", &[("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {answered}"
        );
        let (status, body) = config_until(&router, &function, row.qualifier, 404).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected AWS Lambda to hold no configuration once the stack \
             deleted it, got {status} with body {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: a destination the execution role may not write fails the stack
// ---------------------------------------------------------------------------

/// One row of the role table: the destination the configuration names, what the role is
/// granted, and whether the rule says the stack can then deploy.
struct RoleRow {
    case: &'static str,
    /// The stack, the function and the role the row owns.
    slug: &'static str,
    /// Whether the destination is the row's own QUEUE (which this suite creates) or an Amazon
    /// SNS topic — a destination kind whose write is `sns:Publish`.
    topic: bool,
    /// The actions the role's inline policy grants, on the destination itself.
    granted: &'static [&'static str],
    /// Whether the stack reaches `CREATE_COMPLETE` (so the configuration exists) or is
    /// rolled back.
    deployed: bool,
    /// The call Lambda's refusal must name for a row that is rolled back; the refusal names
    /// the declared destination beside it.
    withheld: Option<&'static str>,
}

/// The rows differ along what the role is granted — nothing at all, another service's write,
/// the write the destination takes — and along the KIND of destination that write is judged
/// for: a standard queue (`sqs:SendMessage`) and a standard topic (`sns:Publish`), two of the
/// kinds `DestinationConfig` names.
const ROLE_ROWS: &[RoleRow] = &[
    RoleRow {
        case: "a queue destination and a role granted nothing",
        slug: "eic-role-none",
        topic: false,
        granted: &[],
        deployed: false,
        withheld: Some("SendMessage"),
    },
    RoleRow {
        case: "a queue destination and a role granted only a topic's write",
        slug: "eic-role-other",
        topic: false,
        granted: &["sns:Publish"],
        deployed: false,
        withheld: Some("SendMessage"),
    },
    RoleRow {
        case: "a queue destination and a role granted the queue's own write",
        slug: "eic-role-granted",
        topic: false,
        granted: &["sqs:SendMessage"],
        deployed: true,
        withheld: None,
    },
    RoleRow {
        case: "a topic destination and a role granted only the queue's write",
        slug: "eic-role-topic",
        topic: true,
        granted: &["sqs:SendMessage"],
        deployed: false,
        withheld: Some("Publish"),
    },
    RoleRow {
        case: "a topic destination and a role granted the topic's own write",
        slug: "eic-role-topic-granted",
        topic: true,
        granted: &["sns:Publish"],
        deployed: true,
        withheld: None,
    },
];

/// The sentence AWS Lambda refuses a configuration whose function execution role may not make
/// the write that delivers a record to the named destination with — the wording that service
/// answers this error class with, naming the call and then the DESTINATION the configuration
/// declared, by its own ARN.
fn refusal_sentence(withheld: &str, destination: &str) -> String {
    format!(
        "The function execution role does not have permissions to call {withheld} on \
         {destination}"
    )
}

#[tokio::test]
async fn a_destination_the_execution_role_cannot_write_rolls_the_stack_back() {
    for row in ROLE_ROWS {
        let router = build_router();
        let case = row.case;
        let function = format!("{}-fn", row.slug);
        let stack = format!("{}-stack", row.slug);
        // The destination the configuration names. A topic is named by its ARN alone: the
        // decision is made on the policies in force on the role, so nothing is asked of the
        // destination's own service.
        let destination = if row.topic {
            format!("arn:aws:sns:{REGION}:{ACCOUNT}:{}-records", row.slug)
        } else {
            given_queue(&router, case, &format!("{}-records", row.slug))
                .await
                .1
        };
        let role = given_role(
            &router,
            case,
            &format!("{}-exec", row.slug),
            row.granted,
            std::slice::from_ref(&destination),
        )
        .await;
        let body = template(
            &function,
            &role,
            UNPUBLISHED,
            &json!({
                "DestinationConfig": { "OnFailure": { "Destination": destination } },
                "MaximumRetryAttempts": 0,
            }),
        );

        let (status, answered) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", &stack), ("TemplateBody", &body)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
        );

        let wanted = if row.deployed {
            CREATE_COMPLETE
        } else {
            ROLLBACK_COMPLETE
        };
        let (reached, described) = wait_for_stack_status(&router, &stack, wanted).await;
        assert_eq!(
            reached, wanted,
            "case {case:?}: expected the stack to reach {wanted}, got {described}"
        );

        let (status, held) = lambda(
            &router,
            "GET",
            &config_uri(&function, UNPUBLISHED),
            None,
            &[],
        )
        .await;
        if row.deployed {
            assert_eq!(
                status, 200,
                "case {case:?}: expected AWS Lambda to hold the configuration the stack \
                 declared, got {status} with body {held}"
            );
            assert_eq!(
                held["DestinationConfig"]["OnFailure"]["Destination"].as_str(),
                Some(destination.as_str()),
                "case {case:?}: expected the declared destination, got {held}"
            );
        } else {
            assert_eq!(
                status, 404,
                "case {case:?}: expected no configuration to be put for a role that cannot \
                 write the destination, got {status} with body {held}"
            );
            let (status, events) =
                cloudformation(&router, "DescribeStackEvents", &[("StackName", &stack)]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with \
                 body {events}"
            );
            let sentence = refusal_sentence(
                row.withheld
                    .expect("a rolled-back row names the call its role may not make"),
                &destination,
            );
            let reasons = elements(&events, "ResourceStatusReason");
            assert!(
                reasons.iter().any(|reason| reason.contains(&sentence)),
                "case {case:?}: expected a resource failure carrying Lambda's refusal \
                 {sentence:?}, got {reasons:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 3: the invocation record of a completed event reaches the declared slot
// ---------------------------------------------------------------------------

/// One row of the delivery table: the outcome the event reaches, the slot that must receive
/// its invocation record, and what the record must report about the run.
struct DeliveryRow {
    case: &'static str,
    /// Whether the event asks the handler to RAISE.
    fails: bool,
    /// The slot the record must be delivered to: `true` for the on-failure queue.
    on_failure: bool,
    /// The `requestContext.condition` the record must carry — the documented condition of
    /// the outcome the event reached.
    condition: &'static str,
}

/// The rows differ along the outcome the event reached, which is what decides the slot its
/// record goes to.
const DELIVERY_ROWS: &[DeliveryRow] = &[
    DeliveryRow {
        case: "an event whose handler raises",
        fails: true,
        on_failure: true,
        condition: "RetriesExhausted",
    },
    DeliveryRow {
        case: "an event whose handler returns",
        fails: false,
        on_failure: false,
        condition: "Success",
    },
];

#[tokio::test]
async fn the_record_of_a_completed_event_reaches_the_destination_the_stack_declared() {
    let router = build_router();
    let function = "eic-delivery-fn";
    let stack = "eic-delivery-stack";
    let case = "the stack both rows are deployed by";
    let (success_url, success_arn) = given_queue(&router, case, "eic-delivery-success").await;
    let (failure_url, failure_arn) = given_queue(&router, case, "eic-delivery-failure").await;
    let role = given_role(
        &router,
        case,
        "eic-delivery-exec",
        &["sqs:SendMessage"],
        &[success_arn.clone(), failure_arn.clone()],
    )
    .await;
    // No retries, so each event has exactly one delivery attempt and the record's
    // `approximateInvokeCount` is a known number.
    let body = template(
        function,
        &role,
        UNPUBLISHED,
        &json!({
            "DestinationConfig": {
                "OnSuccess": { "Destination": success_arn },
                "OnFailure": { "Destination": failure_arn },
            },
            "MaximumRetryAttempts": 0,
        }),
    );
    let (status, answered) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &body)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );
    let (reached, described) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the stack to deploy, got {described}"
    );

    for row in DELIVERY_ROWS {
        let case = row.case;
        let event = json!({ "fail": row.fails, "case": case });
        let (status, answered) = lambda(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            Some(&event),
            &[(INVOCATION_TYPE_HEADER, EVENT_TYPE)],
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected an asynchronous Invoke to be accepted, got {status} with \
             body {answered}"
        );

        let url = if row.on_failure {
            &failure_url
        } else {
            &success_url
        };
        let held = awaited(&router, case, url, 1).await;
        let record: Value = serde_json::from_str(held[0]["Body"].as_str().unwrap_or_default())
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the record is not a document: {error} in {held:?}")
            });
        // (the member the record carries, the value it must carry)
        //
        // The function ARN a record names is QUALIFIED by the version the event was executed
        // on, as the documented shape of an invocation record carries it
        // ("functionArn": "arn:…:function:<name>:$LATEST").
        let stated: &[(&str, Value)] = &[
            (
                "/requestContext/functionArn",
                json!(format!("{}:{UNPUBLISHED}", function_arn(function))),
            ),
            ("/requestContext/condition", json!(row.condition)),
            ("/requestContext/approximateInvokeCount", json!(1)),
            ("/requestPayload", event.clone()),
        ];
        for (pointer, expected) in stated {
            let reported = record.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                &reported, expected,
                "case {case:?}: expected {pointer} to be {expected}, got {record}"
            );
        }
        let request_id = record
            .pointer("/requestContext/requestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !request_id.is_empty(),
            "case {case:?}: expected the record to name the request it is about, got {record}"
        );
    }
}
