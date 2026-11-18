// SPDX-License-Identifier: CC0-1.0

//! What an `AWS::Lambda::EventSourceMapping` whose create AWS Lambda refused reports on its own
//! `CREATE_FAILED` stack event.
//!
//! RULE under test: **a resource provider that words its refusals through the CloudFormation
//! resource-provider plugin's exception classes carries the CLASS's own wording inside the
//! message CloudFormation quotes — a refusal the provider classified as the plugin's
//! invalid-request failure reaches the resource's `ResourceStatusReason` as `Invalid request
//! provided: <the exception the owning service's SDK raised>`, whichever call of that service
//! was refused and whatever the service's own sentence says — while a refusal of any OTHER class
//! carries no such wording.** The mapping's provider is one of those: the quoted message opens
//! with that wording, then AWS Lambda's own sentence, then its SDK's diagnostic of the refused
//! call (the SDK's name for the service, the status it answered, the id of the call and the
//! attempts the SDK made), ahead of the `RequestToken` and `HandlerErrorCode` suffix
//! CloudFormation adds.
//!
//! Why it matters: a client — a deployment tool, a CI log scraper, a support runbook — matches
//! CloudFormation's documented reason text to classify a rolled-back deployment as an
//! invalid-request handler error. A reason whose quote opens straight at the owning service's
//! sentence matches none of those patterns, so a failure with a known class is reported as
//! unknown.
//!
//! Four crates have to agree for the rule to hold (CloudFormation provisions the mapping and
//! words the reason, AWS Lambda owns the mapping and refuses the create, Amazon SQS holds the
//! queue the mapping would poll and IAM answers for the execution role it would read it as), so
//! the suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: WHAT Lambda refused (an execution role
//! that may not read the queue — the recorded case — a batch larger than the queue's ceiling with
//! no batching window, and an event source of a service no mapping polls), hence WHICH sentence
//! that service words and at which point of the call it refused, and the CLASS the refusal falls
//! in (the last row names a function Lambda does not hold, which is the not-found class and
//! carries no plugin wording). The CONTROL deploys the same estate with nothing wrong with it:
//! the mapping is created, the stack completes, no resource records a failure and Lambda itself
//! reports the mapping polling the declared queue — so the wording above is what a REFUSAL
//! reports rather than something every deployment of the type now carries.
//!
//! Every expected value is computed here from what the row declares and from the owning service's
//! own refusal constants — never read back out of the answer being checked, except the UUID
//! Lambda mints for the mapping, which is that service's to assign.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery CloudFormation caller, the Lambda restJson1 caller, the stack read and the XML
// readers are each written ONCE in this test crate: a suite that copies one drifts from the
// others.
use crate::cloudformation_apigatewayv2_resources::{cloudformation, REGION};
use crate::cloudformation_lambda_ssm_sourced_update::lambda;
use crate::cloudformation_provisions_lambda_event_source_mappings::{
    MAPPINGS_URI, MAPPING_STATE_ENABLED,
};
use crate::query_wire::{elements, members_of, opt_element, physical_ids_of};

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The SDK's own name for AWS Lambda, as the exception its SDK raises states it.
const LAMBDA_SERVICE: &str = "Lambda";

/// The wording the plugin's invalid-request exception opens the quoted message with.
const INVALID_REQUEST_PROVIDED: &str = "Invalid request provided: ";

/// The status AWS Lambda refuses a `CreateEventSourceMapping` whose request it will not take
/// with: `InvalidParameterValueException`'s own `httpError`
/// (`smithy_query(query="operation",operation="CreateEventSourceMapping")`).
const REFUSED_PARAMETER_STATUS: u16 = 400;

/// The status it answers for a function it does not hold: `ResourceNotFoundException`'s own
/// `httpError` in the same operation.
const NOT_FOUND_STATUS: u16 = 404;

/// The `HandlerErrorCode` classes the refusals below fall in, both values of the CloudFormation
/// model's own `HandlerErrorCode` enum.
const INVALID_REQUEST: &str = "InvalidRequest";
const NOT_FOUND: &str = "NotFound";

/// The statuses a stack reports once a refused resource has been rolled back, and the status the
/// refused resource's own event carries.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The logical ids every template below declares its resources under.
const QUEUE_ID: &str = "Source";
const ROLE_ID: &str = "ExecRole";
const FUNCTION_ID: &str = "Consumer";
const MAPPING_ID: &str = "Trigger";

/// How long a wait for a stack to settle, or for an event to be recorded, is given.
///
/// Generous on purpose: what is waited FOR is the deployment settling, which is on the stack's
/// own clock, and every wait below ends as soon as the condition holds rather than after a fixed
/// span.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
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

/// The queue every template declares, named for the stack that declares it.
fn queue_name(stack: &str) -> String {
    format!("{stack}-queue")
}

/// The ARN Amazon SQS addresses that queue by, computed from the Region, the account and the
/// name.
fn queue_arn(stack: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{}", queue_name(stack))
}

/// The template a row deploys: the queue the mapping would poll, the execution role the poller
/// would read it as (granted `granted`), the function the mapping invokes, and the mapping
/// itself, declared with `mapping` as its properties.
///
/// `with_function` is `false` for a row whose mapping names a function nothing declares, which
/// then declares neither the function nor the role it would execute as.
fn template(stack: &str, granted: &[&str], mapping: Value, with_function: bool) -> String {
    let mut resources = json!({
        QUEUE_ID: {
            "Type": "AWS::SQS::Queue",
            "Properties": { "QueueName": queue_name(stack) },
        },
        MAPPING_ID: {
            "Type": "AWS::Lambda::EventSourceMapping",
            "Properties": mapping,
        },
    });
    if with_function {
        let declared = resources
            .as_object_mut()
            .expect("the resources section is an object");
        declared.insert(
            ROLE_ID.to_string(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {
                                "Service":
                                    service_com_amazonaws_lambda_awsgirapiservice::state::EXECUTION_ROLE_PRINCIPAL,
                            },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "granted-reads",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": granted,
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            }),
        );
        declared.insert(
            FUNCTION_ID.to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{stack}-fn"),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE_ID, "Arn"]},
                    "Code": {"ZipFile": "def handler(event, context):\n    return {}\n"},
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
    })
    .to_string()
}

/// The reads of a QUEUE the poller makes on the named source, as AWS Lambda's own refusal of a
/// role that may not make them lists them — granted to the role of every row the queue's reads
/// are not what is under test.
const QUEUE_READS: &[&str] = &[
    "sqs:ReceiveMessage",
    "sqs:DeleteMessage",
    "sqs:GetQueueAttributes",
];

/// A grant of something other than the queue's reads: the role has a document in force, and
/// none of it lets the poller read the source.
const UNRELATED_GRANT: &[&str] = &["logs:CreateLogStream"];

/// The sentence AWS Lambda words for a role that may not make the first of the queue's reads.
///
/// Source: that service's own refusal of a queue event source whose reads the execution role is
/// not granted (`state::event_source_authorization`, whose queue entry names `ReceiveMessage`
/// first and words the refusal around the call and the service it would have been made to). It
/// is fixed text but for those two, so it is compared exactly.
const ROLE_WITHOUT_RECEIVE_MESSAGE: &str =
    "The function execution role does not have permissions to call ReceiveMessage on SQS";

/// Submit `template` as `stack`, acknowledging the IAM resources it declares, and assert the
/// submission itself was accepted.
async fn create_stack(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
}

/// Poll `stack` until its status is no longer an `*_IN_PROGRESS` one, and answer the status it
/// settled in.
///
/// The wait is on that CONDITION — the deployment reaching a terminal status, which is owed from
/// the instant the owning services have answered for every resource — rather than on a fixed
/// span, so it ends as soon as the stack has settled.
async fn settled_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") {
            return reached;
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {stack} was still {reached:?} after {SETTLE_DEADLINE:?}, so nothing \
             settled it: {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `ResourceStatusReason` of the event `logical_id` reached `reported` in, or `None` where
/// the stack has recorded no such event.
///
/// Read from the `<member>` that carries the logical id, the status and the reason together, so
/// a reason is never read off another resource's — or another transition's — event.
async fn recorded_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> Option<String> {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body {body}"
    );
    members_of(&body).into_iter().find_map(|member| {
        let named = elements(member, "LogicalResourceId")
            .into_iter()
            .next()
            .is_some_and(|named| named == logical_id);
        let failed = elements(member, "ResourceStatus")
            .into_iter()
            .next()
            .is_some_and(|status| status == reported);
        (named && failed)
            .then(|| elements(member, "ResourceStatusReason").into_iter().next())
            .flatten()
            .filter(|reason| !reason.is_empty())
    })
}

/// The same reason, looked for again until the stack's own clock has recorded it or
/// [`SETTLE_DEADLINE`] passes.
async fn failure_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> String {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        if let Some(reason) = recorded_reason(router, case, stack, logical_id, reported).await {
            return reason;
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {logical_id} reported no {reported} reason within \
             {SETTLE_DEADLINE:?}; the stack recorded {:?}",
            recorded_reasons(router, stack).await
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every reason the stack recorded, for an assertion that has to say where a resource failed
/// instead.
async fn recorded_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&body, "ResourceStatusReason")
}

/// The provider exception a `ResourceStatusReason` quotes, and the `HandlerErrorCode` class it
/// reports beside it.
fn quoted_and_class(case: &str, reason: &str) -> (String, String) {
    const OPENS: &str = "Resource handler returned message: \"";
    const TOKEN: &str = "\" (RequestToken: ";
    const CLASS: &str = "HandlerErrorCode: ";
    let rest = reason.strip_prefix(OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open with {OPENS:?}, got {reason:?}")
    });
    let (quoted, tail) = rest.split_once(TOKEN).unwrap_or_else(|| {
        panic!("case {case:?}: expected a quoted exception and a RequestToken, got {reason:?}")
    });
    let class = tail
        .split_once(CLASS)
        .and_then(|(_, class)| class.strip_suffix(')'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a HandlerErrorCode, got {reason:?}"));
    (quoted.to_string(), class.to_string())
}

/// Hold one SDK exception to the shape an AWS Lambda SDK raises: the service's own sentence,
/// then the diagnostic of the refused call — the SDK's name for the service, the HTTP status it
/// answered, the id of the call and the attempts the SDK made at it. Answers the sentence.
fn asserted_sentence(case: &str, exception: &str, status: u16) -> String {
    const DIAGNOSTIC: &str = " (Service: ";
    const ATTEMPTS: &str = ") (SDK Attempt Count: ";
    let (sentence, diagnostic) = exception.rsplit_once(DIAGNOSTIC).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK diagnostic inside the quoted exception, after the \
             service's own sentence, got {exception:?}"
        )
    });
    assert!(
        !sentence.trim().is_empty(),
        "case {case:?}: expected the service's own sentence ahead of the diagnostic, got \
         {exception:?}"
    );
    let (envelope, attempts) = diagnostic.split_once(ATTEMPTS).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the attempt count this service's SDK words after the \
             envelope, got {exception:?}"
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
        fields[0], LAMBDA_SERVICE,
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {status}"),
        "case {case:?}: expected the status the owning service answered, got {:?}",
        fields[1]
    );
    let request_id = fields[2]
        .strip_prefix("Request ID: ")
        .unwrap_or_else(|| panic!("case {case:?}: expected a Request ID, got {:?}", fields[2]));
    assert!(
        is_a_request_id(request_id),
        "case {case:?}: expected the id of the call in the form that service's answers carry, \
         got {request_id:?}"
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

/// Whether `value` has the 8-4-4-4-12 lowercase-hexadecimal form an AWS request id carries.
fn is_a_request_id(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|group| {
            group
                .chars()
                .all(|held| held.is_ascii_hexdigit() && !held.is_ascii_uppercase())
        })
}

/// One row: the stack to deploy, what the mapping declares, and what the failed resource's
/// reason has to report.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: String,
    /// The sentence AWS Lambda itself words for the refusal, which the SDK exception opens with.
    says: String,
    /// The `HandlerErrorCode` class the refusal falls in.
    class: &'static str,
    /// The HTTP status the owning service answered.
    status: u16,
    /// Whether the plugin's own invalid-request wording opens the quoted message — true for the
    /// class that provider words through it, false for every other class.
    plugin_worded: bool,
}

fn rows() -> Vec<Row> {
    let unpollable = format!("arn:aws:s3:{REGION}:{ACCOUNT}:lambda-esm-reason-not-a-source");
    let absent_function = "lambda-esm-reason-absent-fn";
    vec![
        Row {
            // The recorded shape: the queue is there, the mapping is well formed, and the role
            // the poller would read the queue as is not granted the read.
            case: "an execution role that may not read the queue the mapping polls",
            stack: "lambda-esm-reason-role",
            template: template(
                "lambda-esm-reason-role",
                UNRELATED_GRANT,
                json!({
                    "EventSourceArn": {"Fn::GetAtt": [QUEUE_ID, "Arn"]},
                    "FunctionName": {"Ref": FUNCTION_ID},
                    "BatchSize": 1,
                }),
                true,
            ),
            says: ROLE_WITHOUT_RECEIVE_MESSAGE.to_string(),
            class: INVALID_REQUEST,
            status: REFUSED_PARAMETER_STATUS,
            plugin_worded: true,
        },
        Row {
            // Another refusal of the same class, at another point of the same call: the batch
            // the mapping asks for is past what a queue hands over without a batching window.
            case: "a batch past the queue's ceiling with no batching window",
            stack: "lambda-esm-reason-batch",
            template: template(
                "lambda-esm-reason-batch",
                QUEUE_READS,
                json!({
                    "EventSourceArn": {"Fn::GetAtt": [QUEUE_ID, "Arn"]},
                    "FunctionName": {"Ref": FUNCTION_ID},
                    "BatchSize":
                        service_com_amazonaws_lambda_awsgirapiservice::state::MAX_QUEUE_BATCH_SIZE_WITHOUT_WINDOW
                            + 1,
                }),
                true,
            ),
            says: service_com_amazonaws_lambda_awsgirapiservice::state::QUEUE_BATCH_WINDOW_MESSAGE
                .to_string(),
            class: INVALID_REQUEST,
            status: REFUSED_PARAMETER_STATUS,
            plugin_worded: true,
        },
        Row {
            // A third refusal of the same class: a well-formed ARN naming a resource of a
            // service no event source mapping polls.
            case: "an event source of a service no mapping polls",
            stack: "lambda-esm-reason-source",
            template: template(
                "lambda-esm-reason-source",
                QUEUE_READS,
                json!({
                    "EventSourceArn": unpollable.clone(),
                    "FunctionName": {"Ref": FUNCTION_ID},
                    "BatchSize": 1,
                }),
                true,
            ),
            says: service_com_amazonaws_lambda_awsgirapiservice::state::unrecognized_event_source(
                "s3",
                &unpollable,
                REGION,
            ),
            class: INVALID_REQUEST,
            status: REFUSED_PARAMETER_STATUS,
            plugin_worded: true,
        },
        Row {
            // The other class: the mapping names a function Lambda does not hold, which is the
            // ABSENCE of the resource the declaration named rather than a request that service
            // refused — so the plugin's invalid-request wording is no part of it.
            case: "a function AWS Lambda does not hold",
            stack: "lambda-esm-reason-function",
            template: template(
                "lambda-esm-reason-function",
                UNRELATED_GRANT,
                json!({
                    "EventSourceArn": {"Fn::GetAtt": [QUEUE_ID, "Arn"]},
                    "FunctionName": absent_function,
                    "BatchSize": 1,
                }),
                false,
            ),
            says: format!(
                "Function not found: arn:aws:lambda:{REGION}:{ACCOUNT}:function:{absent_function}"
            ),
            class: NOT_FOUND,
            status: NOT_FOUND_STATUS,
            plugin_worded: false,
        },
    ]
}

/// Every row: the refused mapping fails, the stack rolls back, and the failed resource's reason
/// carries the plugin's invalid-request wording exactly where the refusal falls in that class —
/// around AWS Lambda's own sentence and its SDK's diagnostic of the call.
#[tokio::test]
async fn a_refused_mapping_reports_the_class_its_provider_worded_the_refusal_through() {
    for row in rows() {
        let case = row.case;
        let router = build_router();
        create_stack(&router, case, row.stack, &row.template).await;

        let settled = settled_status(&router, case, row.stack).await;
        assert_eq!(
            settled, ROLLBACK_COMPLETE,
            "case {case:?}: expected the refused mapping to roll the stack back, got \
             {settled:?} with the reasons {:?}",
            recorded_reasons(&router, row.stack).await
        );

        let reason =
            failure_reason(&router, case, row.stack, MAPPING_ID, CREATE_FAILED).await;
        let (quoted, class) = quoted_and_class(case, &reason);
        assert_eq!(
            class, row.class,
            "case {case:?}: expected the {:?} class, got {class:?} in {reason:?}",
            row.class
        );
        let exception = if row.plugin_worded {
            quoted.strip_prefix(INVALID_REQUEST_PROVIDED).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the quoted message to open with \
                     {INVALID_REQUEST_PROVIDED:?}, got {quoted:?}"
                )
            })
        } else {
            assert!(
                !quoted.contains(INVALID_REQUEST_PROVIDED),
                "case {case:?}: expected no invalid-request wording on a refusal of another \
                 class, got {quoted:?}"
            );
            quoted.as_str()
        };
        let sentence = asserted_sentence(case, exception, row.status);
        assert_eq!(
            sentence, row.says,
            "case {case:?}: expected the sentence AWS Lambda words for what it refused, got \
             {sentence:?} in {reason:?}"
        );
    }
}

/// The CONTROL: the same estate with nothing wrong with it. Nothing refuses the mapping, so the
/// stack completes, no resource records a failure, and AWS Lambda itself reports the mapping
/// polling the queue the template declared — which is what makes each row above a refusal rather
/// than a deployment that fails whatever it declares.
#[tokio::test]
async fn a_mapping_lambda_accepts_is_created_and_reports_no_refusal() {
    let case = "a mapping whose role is granted the queue's reads";
    let stack = "lambda-esm-reason-accepted";
    let router = build_router();
    create_stack(
        &router,
        case,
        stack,
        &template(
            stack,
            QUEUE_READS,
            json!({
                "EventSourceArn": {"Fn::GetAtt": [QUEUE_ID, "Arn"]},
                "FunctionName": {"Ref": FUNCTION_ID},
                "BatchSize": 1,
            }),
            true,
        ),
    )
    .await;

    let settled = settled_status(&router, case, stack).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "case {case:?}: expected the accepted mapping to complete the stack, got {settled:?} \
         with the reasons {:?}",
        recorded_reasons(&router, stack).await
    );
    let failed = recorded_reason(&router, case, stack, MAPPING_ID, CREATE_FAILED).await;
    assert!(
        failed.is_none(),
        "case {case:?}: expected no refused mapping, got {failed:?}"
    );

    // The mapping the stack published is one AWS Lambda really holds, polling the declared
    // queue: the refusals above are about what that service refuses, not about the type.
    let (status, resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {resources}"
    );
    let uuid = physical_ids_of(&resources)
        .remove(MAPPING_ID)
        .unwrap_or_else(|| {
            panic!("case {case:?}: no physical id for {MAPPING_ID} in {resources}")
        });
    let (status, _, reported) =
        lambda(&router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetEventSourceMapping to resolve the id the stack published \
         ({uuid:?}), got {status} with body {reported}"
    );
    assert_eq!(
        reported.get("State").and_then(Value::as_str),
        Some(MAPPING_STATE_ENABLED),
        "case {case:?}: expected the created mapping to be polling, got {reported}"
    );
    assert_eq!(
        reported.get("EventSourceArn").and_then(Value::as_str),
        Some(queue_arn(stack).as_str()),
        "case {case:?}: expected the mapping to poll the declared queue, got {reported}"
    );
}
