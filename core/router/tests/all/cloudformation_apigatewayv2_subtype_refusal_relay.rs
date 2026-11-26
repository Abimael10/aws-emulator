// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack REPORTS when API Gateway v2 refuses the integration a template
//! declares for one of its first-class service actions — over the service set the recorded
//! deployment spans (CloudFormation resolves the declaration and makes the calls, Amazon SQS
//! mints the queue the mapping points at, IAM holds the role the integration sends under, and
//! API Gateway v2 refuses the create), which is the only place the whole reason can be read.
//!
//! The RULE pinned here, in two halves, both read off the one `ResourceStatusReason` a client
//! triaging the failed deploy sees:
//!
//! 1. **The refusal lists the subtype's WHOLE required set in the order API Gateway publishes
//!    for THAT subtype, never a sorted one.** The sentence is fixed but for the subtype the
//!    template named and the bracketed list, and the order of that list is a per-subtype fact:
//!    two actions of one service order the same two parameters oppositely, so nothing may
//!    sort, re-group or filter the list on its way into the stack event.
//! 2. **The SDK diagnostic that follows names the service that SERVED the refused call, by that
//!    service's own SDK id, and counts the attempts the SDK made** — `(Service: <service>,
//!    Status Code: <status>, Request ID: <id>) (SDK Attempt Count: <n>)` — inside the handler
//!    envelope `Resource handler returned message: "…" (RequestToken: <token>,
//!    HandlerErrorCode: <class>)`. API Gateway v2 and the API Gateway REST control plane SHARE
//!    a signing name and are different services, so a refusal of a v2 call must carry the v2
//!    SDK id rather than the REST plane's.
//!
//! Why it matters: the stack event is the only account of the failure a client has. A
//! parameter list in an order AWS never emits defeats any pipeline that matches the
//! `CREATE_FAILED` reason, and a trailer naming the wrong service — or one that stops before
//! the attempt count — sends an operator, and anything parsing the trailer, to the wrong
//! service's logs.
//!
//! The rows differ along what the rule generalises over: the SUBTYPE the template names, which
//! is what the order is a fact about — from a one-parameter action to a four-parameter one,
//! including two actions of ONE service whose two parameters are published the opposite way
//! round and three whose published order is not the alphabetical one — and how many of the
//! required parameters the declaration leaves out.
//!
//! Scope: what API Gateway v2's own API answers such a create with is that crate's
//! `integration_validation`; what this suite reaches is the sentence and the SDK trailer as a
//! CloudFormation stack event relays them.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_apigatewayv2_resources::cloudformation;
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_provisions_cleanroomsml_training_datasets::quoted_and_class;
use crate::query_wire::{elements, members_of};

/// The SDK's own name for Amazon API Gateway v2: that model's `sdkId`, which is also the name
/// its `AWS::ApiGatewayV2::*` type names carry.
const APIGATEWAYV2_SDK_NAME: &str = "ApiGatewayV2";

/// The SDK's own name for the API Gateway REST control plane, which SHARES API Gateway v2's
/// signing name and is a different service — the name a v2 refusal must NOT carry.
const APIGATEWAY_REST_SDK_NAME: &str = "ApiGateway";

/// The status API Gateway v2 answers a create it refuses as a bad request with
/// (`smithy_query(query="operation",operation="CreateIntegration")`: `BadRequestException`,
/// `httpError` 400).
const REFUSED_STATUS: u16 = 400;

/// The `HandlerErrorCode` class a refused REQUEST falls in.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The terminal status a stack whose create was refused rests in, and the event status its
/// refused resource records.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The logical id every template below declares its integration under.
const INTEGRATION: &str = "Integration";

/// The protocol of an api whose integrations may name a service action ("Supported only for
/// HTTP API AWS_PROXY integrations", `CreateIntegration$IntegrationSubtype`), and the
/// `PayloadFormatVersion` such an integration declares.
const HTTP: &str = "HTTP";
const PAYLOAD_FORMAT_VERSION: &str = "1.0";

/// The capability a template declaring a NAMED IAM resource has to be created with:
/// "CAPABILITY_NAMED_IAM … if you have IAM resources with custom names"
/// (`CreateStackInput$Capabilities`).
const NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";

/// CloudFormation, API Gateway v2, Amazon SQS and IAM over ONE engine, as
/// `core/router/src/lib.rs` mounts them.
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
    router
}

/// One row: the subtype the template names, the INCOMPLETE mapping it declares for it, and the
/// whole required set the refusal has to list, in that subtype's own published order.
struct Row {
    case: &'static str,
    stack: &'static str,
    subtype: &'static str,
    /// The `RequestParameters` the declaration states — a mapping that leaves at least one
    /// required parameter of the action undefined.
    mapped: fn() -> Value,
    /// The subtype's WHOLE required set, in the order API Gateway publishes for that subtype.
    ///
    /// Written out here rather than read off the service under test, so the ORDER is asserted
    /// against AWS and not merely agreed between the handler and this suite. Source: the
    /// parameter order API Gateway publishes per subtype in its AWS-service integration
    /// reference for HTTP APIs, which is the order its refusal reproduces.
    published: &'static [&'static str],
}

/// The queue the stack declares, and nothing else: for `SQS-SendMessage` that is the recorded
/// shape (no body to send), and for `SQS-DeleteMessage` it leaves out the handle that names the
/// message to delete.
fn only_the_queue_mapped() -> Value {
    json!({ "QueueUrl": { "Ref": "Queue" } })
}

/// A record with a payload but neither the stream it is put on nor the key it is partitioned
/// by: two of three required parameters left out.
fn put_record_without_a_stream() -> Value {
    json!({ "Data": "$request.body" })
}

/// A configuration read that names only the application: three of four required parameters
/// left out.
fn get_configuration_without_its_environment() -> Value {
    json!({ "Application": "$request.path.application" })
}

/// A stop that states a cause — an OPTIONAL argument of the same action — and not the
/// execution to stop, so the refusal is about the required parameter and not about a mapping
/// that defines nothing at all.
fn stop_execution_without_an_execution() -> Value {
    json!({ "Cause": "$request.body.cause" })
}

/// The table this suite spans.
const ROWS: &[Row] = &[
    // The recorded case: two parameters published queue-first, which is NOT their alphabetical
    // order.
    Row {
        case: "an SQS-SendMessage mapping that leaves out the message body",
        stack: "subtype-relay-sendmessage",
        subtype: "SQS-SendMessage",
        mapped: only_the_queue_mapped,
        published: &["QueueUrl", "MessageBody"],
    },
    // The same two names for another action of the SAME service, published the other way
    // round: whatever the order is, it is not a fact about the service or about the names.
    Row {
        case: "an SQS-DeleteMessage mapping that leaves out the receipt handle",
        stack: "subtype-relay-deletemessage",
        subtype: "SQS-DeleteMessage",
        mapped: only_the_queue_mapped,
        published: &["ReceiptHandle", "QueueUrl"],
    },
    // Three parameters, two of them left out, published in an order that is neither
    // alphabetical nor the one the mapping states them in.
    Row {
        case: "a Kinesis-PutRecord mapping that leaves out the stream and the partition key",
        stack: "subtype-relay-putrecord",
        subtype: "Kinesis-PutRecord",
        mapped: put_record_without_a_stream,
        published: &["StreamName", "Data", "PartitionKey"],
    },
    // Four parameters, three of them left out.
    Row {
        case: "an AppConfig-GetConfiguration mapping that leaves out all but the application",
        stack: "subtype-relay-getconfiguration",
        subtype: "AppConfig-GetConfiguration",
        mapped: get_configuration_without_its_environment,
        published: &["Application", "Environment", "Configuration", "ClientId"],
    },
    // A required set of ONE, so the brackets hold a single name and no separator.
    Row {
        case: "a StepFunctions-StopExecution mapping that leaves out the execution",
        stack: "subtype-relay-stopexecution",
        subtype: "StepFunctions-StopExecution",
        mapped: stop_execution_without_an_execution,
        published: &["ExecutionArn"],
    },
];

/// The template a row deploys: the queue its mapping may point at, the role API Gateway calls
/// the action under, the HTTP api, and the integration whose mapping is incomplete.
fn template(row: &Row) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Queue": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": format!("{}-q", row.stack) },
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
                },
            },
            "Api": {
                "Type": "AWS::ApiGatewayV2::Api",
                "Properties": {
                    "Name": format!("{}-api", row.stack),
                    "ProtocolType": HTTP,
                },
            },
            "Integration": {
                "Type": "AWS::ApiGatewayV2::Integration",
                "Properties": {
                    "ApiId": { "Ref": "Api" },
                    "IntegrationType": "AWS_PROXY",
                    "IntegrationSubtype": row.subtype,
                    "PayloadFormatVersion": PAYLOAD_FORMAT_VERSION,
                    "CredentialsArn": { "Fn::GetAtt": ["Role", "Arn"] },
                    "RequestParameters": (row.mapped)(),
                },
            },
        },
    })
    .to_string()
}

/// The `ResourceStatusReason` the stack recorded on `logical_id`'s `CREATE_FAILED` event.
async fn create_failed_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with {body}"
    );
    let first = |member: &str, name: &str| elements(member, name).into_iter().next();
    members_of(&body)
        .into_iter()
        .filter(|member| {
            first(member, "LogicalResourceId").as_deref() == Some(logical_id)
                && first(member, "ResourceStatus").as_deref() == Some(CREATE_FAILED)
        })
        .find_map(|member| first(member, "ResourceStatusReason"))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a {CREATE_FAILED} event for {logical_id:?} carrying a \
                 reason, got {body}"
            )
        })
}

/// Hold the SDK diagnostic that closes `quoted` to the shape an SDK that counts its attempts
/// words — the service as its own SDK names it, the status the call was answered with, the id
/// of that call and the attempts made at it — and answer the refusing service's own sentence
/// before it.
fn asserted_sentence(case: &str, quoted: &str) -> String {
    const DIAGNOSTIC: &str = " (Service: ";
    const ATTEMPTS: &str = ") (SDK Attempt Count: ";
    let (sentence, diagnostic) = quoted.rsplit_once(DIAGNOSTIC).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK diagnostic after the service's own sentence, got \
             {quoted:?}"
        )
    });
    let (envelope, attempts) = diagnostic.split_once(ATTEMPTS).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the attempt count this SDK words after the envelope, got \
             {quoted:?}"
        )
    });
    let fields: Vec<&str> = envelope.split(", ").collect();
    assert_eq!(
        fields.len(),
        3,
        "case {case:?}: expected the service, the status and the call's id and nothing else, got \
         {envelope:?}"
    );
    assert_eq!(
        fields[0], APIGATEWAYV2_SDK_NAME,
        "case {case:?}: expected the refusing service's own SDK id rather than \
         {APIGATEWAY_REST_SDK_NAME}, the service that merely shares its signing name, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {REFUSED_STATUS}"),
        "case {case:?}: expected the status the refusing service answered, got {:?}",
        fields[1]
    );
    assert!(
        fields[2].starts_with("Request ID: ") && fields[2].len() > "Request ID: ".len(),
        "case {case:?}: expected the id of the refused call, got {:?}",
        fields[2]
    );
    let counted = attempts
        .strip_suffix(')')
        .and_then(|counted| counted.parse::<u32>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a whole attempt count, got {attempts:?}")
        });
    assert!(
        counted >= 1,
        "case {case:?}: expected at least the one attempt the SDK was asked for, got {counted}"
    );
    sentence.to_string()
}

/// The sentence API Gateway words for a subtype whose mapping does not define the parameters
/// its action requires, with `listed` inside the brackets.
fn unmapped_parameters_sentence(subtype: &str, listed: &[&str]) -> String {
    format!(
        "Operation: {subtype} requires enabling passthrough, or defining all of the following \
         parameters in the parameter mapping: [{}]",
        listed.join(", ")
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relayed_subtype_refusal_lists_its_parameters_in_order_and_names_api_gateway_v2() {
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

        let (settled, described) =
            wait_for_stack_status(&router, row.stack, ROLLBACK_COMPLETE).await;
        assert_eq!(
            settled, ROLLBACK_COMPLETE,
            "case {case:?}: expected the refused create to roll the stack back, got {settled} \
             with {described}"
        );

        let reason = create_failed_reason(&router, case, row.stack, INTEGRATION).await;
        let (quoted, class) = quoted_and_class(case, &reason);
        assert_eq!(
            class, INVALID_REQUEST,
            "case {case:?}: expected the {INVALID_REQUEST} class, got {class:?} in {reason:?}"
        );

        // Half 2 of the rule: the trailer names the service that served the refused call, by
        // its own SDK id, and counts the SDK's attempts.
        let sentence = asserted_sentence(case, &quoted);

        // Half 1: the sentence lists the subtype's WHOLE required set in that subtype's own
        // published order.
        let expected = unmapped_parameters_sentence(row.subtype, row.published);
        assert_eq!(
            sentence, expected,
            "case {case:?}: expected the relayed sentence {expected:?}, got {sentence:?}"
        );

        // ... and for a subtype whose published order is not the alphabetical one, that is a
        // difference a client SEES: the relayed sentence differs from the one a sorted list
        // would render.
        let mut sorted: Vec<&str> = row.published.to_vec();
        sorted.sort_unstable();
        if sorted.as_slice() != row.published {
            let if_sorted = unmapped_parameters_sentence(row.subtype, &sorted);
            assert_ne!(
                sentence, if_sorted,
                "case {case:?}: expected the published order rather than a sorted one, got \
                 {sentence:?}"
            );
        }
    }
}
