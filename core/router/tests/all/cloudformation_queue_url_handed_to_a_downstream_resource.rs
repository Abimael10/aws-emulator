// SPDX-License-Identifier: CC0-1.0

//! The queue url a CloudFormation stack hands a DOWNSTREAM resource — over the service set
//! mounted together, which is the only place the question can be put at all.
//!
//! The RULE pinned here: **the value an intrinsic function of an `AWS::SQS::Queue` resolves to
//! is the url Amazon SQS itself reports for that queue ("Ref returns the queue URL", and
//! `Fn::GetAtt <Queue>.QueueUrl` reads the same url), so whatever the deployment writes into
//! ANOTHER resource's property is byte for byte the string that service answers a
//! `GetQueueUrl` for the same `QueueName` with — and reading that property back through its own
//! owning service's API answers that same string.** A queue url is the only handle every
//! queue-addressed call takes and a client sends the string it was handed straight back, so a
//! stack that composes the handle itself — from the service's published regional host, say —
//! hands its dependents an address that does not reach the queue, and no join of a stack's
//! value against `GetQueueUrl` can ever agree.
//!
//! Three crates have to agree for that to hold (CloudFormation resolves the declaration and
//! makes the calls, Amazon SQS mints and reports the url, API Gateway v2 holds and reports the
//! property the stack wrote), so the suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: the FORM the template reads the queue
//! in (`Ref`, `Fn::GetAtt <Queue>.QueueUrl`, and a `Ref` composed through `Fn::Sub`), whether
//! the template NAMES the queue or leaves the name to CloudFormation, and whether the queue is
//! standard or FIFO. Every row is judged where a client sees it: against Amazon SQS's own
//! `GetQueueUrl` answer for the same name, and against API Gateway v2's own read of the
//! integration the stack created — and the url is then USED, by sending a message through the
//! exact string the integration carries, so a row asserts a working handle rather than two
//! strings that merely match.
//!
//! Scope: the agreement between the url every Amazon SQS operation reports and the handle a
//! stack publishes in its own `Outputs` is `one_queue_url_reported_by_every_service.rs`; what a
//! redeployment of the same declaration does with that handle is the CloudFormation crate's
//! `redeployed_queue_keeps_its_reported_url`. What only this suite reaches is the value a
//! deployment hands a DEPENDENT resource of another service.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::cloudformation_apigatewayv2_resources::{apigatewayv2, cloudformation};
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{outputs_of, physical_ids_of};
// One Amazon SQS caller for this target, written once in the suite that owns it: a suite that
// re-writes the awsJson1_0 wire drifts from the others.
use crate::sqs_queue_metric_series::sqs;

/// One address this emulator can be SERVED at, which this suite's router announces as its own
/// endpoint.
///
/// A served emulator is the shape every address it hands a client is read in: the url Amazon SQS
/// reports for a queue names the endpoint the call reached, so with a base set that url differs
/// from the one the naming rules would compose against that service's published regional host —
/// which is what makes the rule under test observable at all.
const SERVED_AT: &str = "https://127.0.0.1:9297";

/// The status a stack that created everything it declared rests in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The capability a template declaring a NAMED IAM resource has to be created with:
/// "CAPABILITY_NAMED_IAM … if you have IAM resources with custom names"
/// (`CreateStackInput$Capabilities`).
const NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";

/// The integration subtype every row declares, and the two parameters it requires — the queue
/// first (`SQS-SendMessage` requires `QueueUrl` and `MessageBody`).
const SUBTYPE: &str = "SQS-SendMessage";

/// The mapping expression the declared integration sends as the message body. Its value is
/// immaterial to the rule: it is the second entry of the same map, so a row also asserts that
/// re-pointing the queue's handle left every other entry exactly as the template wrote it.
const MESSAGE_BODY: &str = "$request.body.msg";

/// The `ProtocolType` of the api the integration hangs off, and the `PayloadFormatVersion` an
/// `AWS_PROXY` subtype integration of an HTTP api declares.
const PROTOCOL_TYPE: &str = "HTTP";
const PAYLOAD_FORMAT_VERSION: &str = "1.0";

/// CloudFormation, API Gateway v2, Amazon SQS and IAM over ONE engine, as
/// `core/router/src/lib.rs` mounts them, served at [`SERVED_AT`].
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: every call carries a credential scope so the
    // router can name the service it is for, and no signature is checked.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router.set_url_base(EndpointUrl::parse(SERVED_AT).expect("the suite's base URL must parse"));
    router
}

/// How a row's template reads the url of the queue it declares.
#[derive(Debug, Clone, Copy)]
enum Handle {
    /// `{"Ref": "Queue"}` — "Ref returns the queue URL".
    Ref,
    /// `{"Fn::GetAtt": ["Queue", "QueueUrl"]}` — the attribute of the same name.
    GetAttQueueUrl,
    /// `{"Fn::Sub": "${Queue}"}`, the same `Ref` composed through a substitution, which is how
    /// a template builds a value around a reference.
    SubstitutedRef,
}

impl Handle {
    /// The intrinsic as the template states it.
    fn intrinsic(self) -> Value {
        match self {
            Handle::Ref => json!({ "Ref": "Queue" }),
            Handle::GetAttQueueUrl => json!({ "Fn::GetAtt": ["Queue", "QueueUrl"] }),
            Handle::SubstitutedRef => json!({ "Fn::Sub": "${Queue}" }),
        }
    }
}

/// One row: the queue a stack declares and the form its dependent resource reads it in.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `QueueName` the template states, or `None` to leave the name to CloudFormation.
    queue_name: Option<&'static str>,
    /// Whether the declaration asks for a FIFO queue, whose name ends `.fifo`.
    fifo: bool,
    handle: Handle,
}

/// The table this suite spans.
const ROWS: &[Row] = &[
    // The recorded shape: a named queue read by `Ref` into another resource's property.
    Row {
        case: "Ref of a queue the template names",
        stack: "queue-handle-ref",
        queue_name: Some("queue-handle-ref-q"),
        fifo: false,
        handle: Handle::Ref,
    },
    // The attribute of the same name, on a queue whose name is CloudFormation's to generate —
    // so neither the name nor the url is a value the template states anywhere.
    Row {
        case: "Fn::GetAtt QueueUrl of a queue CloudFormation names",
        stack: "queue-handle-getatt",
        queue_name: None,
        fifo: false,
        handle: Handle::GetAttQueueUrl,
    },
    // A FIFO queue, whose name and url carry the `.fifo` suffix the type requires.
    Row {
        case: "Ref of a FIFO queue the template names",
        stack: "queue-handle-fifo",
        queue_name: Some("queue-handle-fifo-q.fifo"),
        fifo: true,
        handle: Handle::Ref,
    },
    // The same reference composed through `Fn::Sub`, which resolves through the substitution
    // rather than straight out of the declaration.
    Row {
        case: "a Ref composed through Fn::Sub",
        stack: "queue-handle-sub",
        queue_name: Some("queue-handle-sub-q"),
        fifo: false,
        handle: Handle::SubstitutedRef,
    },
];

/// The template a row deploys: the queue, the role API Gateway sends the message under, the
/// HTTP api, and the `SQS-SendMessage` integration whose `RequestParameters` name the queue by
/// the row's own intrinsic.
fn template(row: &Row) -> String {
    let mut queue_properties = json!({});
    if let Some(name) = row.queue_name {
        queue_properties["QueueName"] = json!(name);
    }
    if row.fifo {
        queue_properties["FifoQueue"] = json!(true);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Queue": {
                "Type": "AWS::SQS::Queue",
                "Properties": queue_properties,
            },
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{}-role", row.stack),
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "apigateway.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "send-to-the-declared-queue",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "sqs:SendMessage",
                                "Resource": { "Fn::GetAtt": ["Queue", "Arn"] },
                            }],
                        },
                    }],
                },
            },
            "Api": {
                "Type": "AWS::ApiGatewayV2::Api",
                "Properties": {
                    "Name": format!("{}-api", row.stack),
                    "ProtocolType": PROTOCOL_TYPE,
                },
            },
            "Integration": {
                "Type": "AWS::ApiGatewayV2::Integration",
                "Properties": {
                    "ApiId": { "Ref": "Api" },
                    "IntegrationType": "AWS_PROXY",
                    "IntegrationSubtype": SUBTYPE,
                    "PayloadFormatVersion": PAYLOAD_FORMAT_VERSION,
                    "CredentialsArn": { "Fn::GetAtt": ["Role", "Arn"] },
                    "RequestParameters": {
                        "QueueUrl": row.handle.intrinsic(),
                        "MessageBody": MESSAGE_BODY,
                    },
                },
            },
        },
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "IntegrationId": { "Value": { "Ref": "Integration" } },
            "RoleArn": { "Value": { "Fn::GetAtt": ["Role", "Arn"] } },
            // The handle the row's own intrinsic resolves to, published so the value the
            // deployment resolved is readable beside the one it wrote into the integration.
            "QueueHandle": { "Value": row.handle.intrinsic() },
        },
    })
    .to_string()
}

/// The queue's NAME: the one the template states, else the last path segment of the url the
/// stack published, which is what names a queue in every url Amazon SQS reports.
fn queue_name_of(row: &Row, published: &str) -> String {
    row.queue_name.map(ToString::to_string).unwrap_or_else(|| {
        published
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string()
    })
}

/// The message a row sends through the exact string the integration carries, so the handle is
/// shown to ADDRESS the queue rather than merely to match another string.
fn probe_body(row: &Row) -> String {
    format!("{}-probe", row.stack)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declared_queues_url_reaches_its_dependent_as_amazon_sqs_reports_it() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();

        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &template(row)),
                ("Capabilities.member.1", NAMED_IAM),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
        );
        let (reached, described) =
            wait_for_stack_status(&router, row.stack, CREATE_COMPLETE).await;
        assert_eq!(
            reached, CREATE_COMPLETE,
            "case {case:?}: expected the stack to deploy, got {reached} with {described}"
        );

        let outputs = outputs_of(&described);
        let published = |name: &str| {
            outputs.get(name).cloned().unwrap_or_else(|| {
                panic!("case {case:?}: expected the stack to publish {name}, got {described}")
            })
        };
        let api_id = published("ApiId");
        let integration_id = published("IntegrationId");
        let role_arn = published("RoleArn");
        let queue_handle = published("QueueHandle");

        // A url on the endpoint this emulator is SERVED at, which is the only kind a client can
        // send back — and not the regional host the naming rules would compose. Without this the
        // row could not tell a handle taken from Amazon SQS from one composed here.
        assert!(
            queue_handle.starts_with(&format!("{SERVED_AT}/")),
            "case {case:?}: expected the stack to publish a queue url on the served endpoint \
             {SERVED_AT}, got {queue_handle:?}"
        );

        // What Amazon SQS itself answers for the same queue, which is the value the rule is
        // stated against.
        let name = queue_name_of(row, &queue_handle);
        let answered = sqs(&router, case, "GetQueueUrl", &json!({ "QueueName": name })).await;
        let reported = answered
            .get("QueueUrl")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a QueueUrl for {name:?}, got {answered}")
            })
            .to_string();
        assert_eq!(
            queue_handle, reported,
            "case {case:?}: expected the stack to publish the url Amazon SQS reports for \
             {name:?} ({reported:?}), got {queue_handle:?}"
        );

        // The identity the stack records for the queue is that same url: "Ref returns the queue
        // URL", and `Ref` resolves to the resource's physical id.
        let (status, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             {resources}"
        );
        let recorded = physical_ids_of(&resources).get("Queue").cloned();
        assert_eq!(
            recorded.as_deref(),
            Some(reported.as_str()),
            "case {case:?}: expected the stack to record the queue under {reported:?}, got \
             {recorded:?} in {resources}"
        );

        // ...and the value the deployment wrote into the DEPENDENT resource, read back through
        // the service that owns it.
        let (status, integration) = apigatewayv2(
            &router,
            "GET",
            &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected API Gateway v2 to report the integration the stack \
             created, got {status} with {integration}"
        );
        let carried = (
            integration.get("integrationSubtype").cloned(),
            integration.get("credentialsArn").cloned(),
            integration.get("requestParameters").cloned(),
        );
        let expected = (
            Some(json!(SUBTYPE)),
            Some(json!(role_arn)),
            Some(json!({ "QueueUrl": reported, "MessageBody": MESSAGE_BODY })),
        );
        assert_eq!(
            carried, expected,
            "case {case:?}: expected GetIntegration to read {expected:?}, got {carried:?} in \
             {integration}"
        );

        // The handle the integration carries ADDRESSES the queue: a message sent through that
        // exact string is accepted and comes back off the queue.
        let body = probe_body(row);
        let mut send = json!({ "QueueUrl": reported, "MessageBody": body });
        if row.fifo {
            // A FIFO queue takes a group for its ordering and, with no content-based
            // deduplication configured, a deduplication id of the sender's own
            // (`SendMessage$MessageGroupId`, `SendMessage$MessageDeduplicationId`).
            send["MessageGroupId"] = json!(row.stack);
            send["MessageDeduplicationId"] = json!(body);
        }
        sqs(&router, case, "SendMessage", &send).await;
        let received = sqs(
            &router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": reported, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let bodies: Vec<String> = received
            .get("Messages")
            .and_then(Value::as_array)
            .map(|messages| {
                messages
                    .iter()
                    .filter_map(|message| message.get("Body").and_then(Value::as_str))
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            bodies.contains(&body),
            "case {case:?}: expected the queue the integration addresses to hold {body:?}, got \
             {bodies:?}"
        );
    }
}
