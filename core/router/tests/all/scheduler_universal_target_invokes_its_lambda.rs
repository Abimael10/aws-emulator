// SPDX-License-Identifier: CC0-1.0

//! EventBridge Scheduler's UNIVERSAL target makes the API call its ARN names: here Lambda's
//! own `Invoke`, over the assembled router.
//!
//! RULE: a `Target.Arn` of the form `arn:<partition>:scheduler:::aws-sdk:<service>:<operation>`
//! names a CALL rather than a resource — EventBridge Scheduler "supports templated target that
//! invoke common API operations, as well as universal targets that you can customize to invoke
//! over 6,000 API operations across more than 270 services" (`Target`) — so at each occurrence
//! Scheduler acts as `Target.RoleArn` and makes THAT operation's call with `Target.Input` ("the
//! text, or well-formed JSON, passed to the target") as the operation's own request, put on the
//! wire the way a client's SDK puts that operation's input on it. Whether the request is one
//! the operation ADMITS is therefore the invoked operation's own question, answered by its own
//! front door: a request it serves has the effect it has, and a request it refuses — because no
//! such resource exists, or because a member's own modelled constraint does not admit the value
//! — fails the delivery under the status, the code and the message that operation answered,
//! which is what reaches the queue the schedule names for a failed event.
//!
//! Only a cross-service suite can pin this: the schedule and the occurrence are EventBridge
//! Scheduler's, the function, its invocation and every refusal of one are Lambda's, the
//! execution role is IAM's, and both the queue the handler writes to and the dead-letter queue
//! are Amazon SQS's. The sibling suite
//! [`super::scheduler_universal_target_invalidates_its_distribution`] reads the same rule for a
//! REST-XML service, and is the suite for the execution role's own permissions deciding whether
//! the call is made at all; what only this one reaches is an operation whose request binds a
//! member to the PATH and another as the request's own body, so a value the path cannot carry
//! as it stands is reached here — and so is a handler actually running, which a universal target
//! that resolves but sends nothing cannot fake.
//!
//! The rows differ along what the rule generalises over: the form the `Input` addresses the
//! function by (its bare name, its full ARN, the partial `<account>:function:<name>` form — the
//! three forms `NamespacedFunctionName` documents), the shape of the payload it carries (a flat
//! object, one with a nested member, one carrying a list), and what the invoked operation
//! answers the request (it serves it, it holds no such function, or the value is outside the
//! pattern its own member declares).
//!
//! Nothing here waits for a fixed span: every schedule names an instant seconds ahead, and the
//! suite watches for the CONDITION — the messages the sink queue holds, the messages the
//! dead-letter queue holds — until a generous deadline.

use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::lambda_handler_invalidates_its_distribution::{
    build_router, call, query, stack_status, CREATE_COMPLETE, REGION,
};
use crate::scheduler_universal_target_invalidates_its_distribution::{
    dead_lettered, given_execution_role, given_queue, given_schedule, ACCOUNT, LOOK_INTERVAL,
    PATIENCE,
};

/// The universal target every row names: Lambda's `Invoke`, written the way the AWS SDK
/// integration family names an operation — the SDK SERVICE ID and that SDK's own operation
/// name in lowerCamelCase.
const UNIVERSAL_TARGET: &str = "arn:aws:scheduler:::aws-sdk:lambda:invoke";

/// The stack that declares the function and the role it runs as, and the function's name.
const FUNCTION_STACK: &str = "xsvc-scheduler-universal-lambda-stack";
const FUNCTION: &str = "scheduler-universal-invokee";

/// The role the function itself runs as, which is NOT the schedule's execution role: the
/// handler's own write is judged against this one.
const FUNCTION_ROLE: &str = "scheduler-universal-function-role";

/// The queue the handler writes to, which is where an invocation is observed, and the queue
/// the schedules name for their failed events.
const SINK_QUEUE: &str = "scheduler-universal-lambda-sink";
const DEAD_LETTER_QUEUE: &str = "scheduler-universal-lambda-failures";

/// A function name no `CreateFunction` created, which is what the row naming a resource the
/// invoked operation holds nothing for carries.
const ABSENT_FUNCTION: &str = "scheduler-universal-absent-function";

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// The error code Lambda answers a request naming a function it does not hold with
/// (`ResourceNotFoundException`, HTTP 404).
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// The error code AWS answers a request whose member value its own modelled constraint does
/// not admit with (`ValidationException`, HTTP 400): `NamespacedFunctionName` declares a
/// `@pattern`, so a `FunctionName` outside it is refused before any function is looked up.
const VALIDATION_CODE: &str = "ValidationException";

/// The handler the stack deploys: it forwards the marker of the payload it was invoked with to
/// the sink queue, which is where a client observes that the invocation happened at all.
///
/// The queue is addressed by NAME, resolved through Amazon SQS's own `GetQueueUrl`, so the
/// source carries nothing the deployment had to compute.
const FORWARDING_HANDLER: &str = r#"
import boto3


def lambda_handler(event, context):
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName="SINK_QUEUE_NAME")["QueueUrl"]
    client.send_message(QueueUrl=url, MessageBody=event["marker"])
    return {"forwarded": event["marker"]}
"#;

/// The stack the function is deployed by: the role it runs as — trusted by Lambda and allowed
/// the forward its handler makes — and the function itself, whose inline source CloudFormation
/// archives into the deployment package.
fn function_stack() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "ForwarderRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": FUNCTION_ROLE,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "forward-to-the-sink",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["sqs:GetQueueUrl", "sqs:SendMessage"],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Forwarder": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": { "Fn::GetAtt": ["ForwarderRole", "Arn"] },
                    "Timeout": 60,
                    "Code": {
                        "ZipFile": FORWARDING_HANDLER.replace("SINK_QUEUE_NAME", SINK_QUEUE),
                    },
                },
            },
        },
        "Outputs": {
            "FunctionName": { "Value": { "Ref": "Forwarder" } },
        },
    })
    .to_string()
}

/// HOW one row's `Input` addresses the function it invokes.
///
/// Lambda documents three forms for the member every one of its function operations takes:
/// `my-function[:qualifier]`, the function's ARN, and the partial
/// `<account>:function:my-function[:qualifier]`, each written inside the pattern the member
/// declares
/// (`smithy_query(query="shape",shape="NamespacedFunctionName",model_path="services/
/// service-com-amazonaws-lambda-awsgirapiservice/smithy-model/
/// com-amazonaws-lambda-awsgirapiservice.json")`). Each of the three names the same function,
/// so a universal target carrying any of them invokes it; a value outside the pattern names
/// none, which is the operation's own refusal to give.
#[derive(Debug, Clone, Copy)]
enum Addressed {
    /// The bare function name.
    Name,
    /// The function's own ARN.
    Arn,
    /// The partial `<account>:function:<name>` form.
    PartialArn,
    /// A bare name, in the documented form, that no function was created under.
    Absent,
    /// Whitespace, which the member's own pattern does not admit — and which the request's
    /// path cannot carry as it stands either.
    Whitespace,
    /// A name carrying a space and a character the member's pattern does not admit.
    Unpatterned,
}

impl Addressed {
    /// The `FunctionName` a row's request carries for the function `function`.
    fn function_name(self, function: &str) -> String {
        match self {
            Self::Name => function.to_string(),
            Self::Arn => function_arn(function),
            Self::PartialArn => format!("{ACCOUNT}:function:{function}"),
            Self::Absent => ABSENT_FUNCTION.to_string(),
            Self::Whitespace => "   ".to_string(),
            Self::Unpatterned => "bad name!".to_string(),
        }
    }
}

/// This account's ARN for the function `name` in the Region the suite drives.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// One row: how the `Input` names the function, the payload it hands it, and what the invoked
/// operation answers.
struct Row {
    case: &'static str,
    addressed: Addressed,
    /// The marker the payload carries, which the handler forwards to the sink queue and which
    /// therefore identifies this row's invocation.
    marker: &'static str,
    /// The error code the invoked operation refuses this request with, or `None` for a request
    /// it serves.
    refusal: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "the function's own name, with a flat payload",
        addressed: Addressed::Name,
        marker: "universal-by-name",
        refusal: None,
    },
    Row {
        case: "the function's full ARN, with a payload carrying a nested member",
        addressed: Addressed::Arn,
        marker: "universal-by-arn",
        refusal: None,
    },
    Row {
        case: "the partial ARN form, with a payload carrying a list",
        addressed: Addressed::PartialArn,
        marker: "universal-by-partial-arn",
        refusal: None,
    },
    Row {
        case: "a documented name the invoked operation holds no function under",
        addressed: Addressed::Absent,
        marker: "universal-absent-function",
        refusal: Some(NOT_FOUND_CODE),
    },
    Row {
        case: "a FunctionName of whitespace, which the member's own pattern does not admit",
        addressed: Addressed::Whitespace,
        marker: "universal-whitespace-name",
        refusal: Some(VALIDATION_CODE),
    },
    Row {
        case: "a FunctionName carrying characters the member's own pattern does not admit",
        addressed: Addressed::Unpatterned,
        marker: "universal-unpatterned-name",
        refusal: Some(VALIDATION_CODE),
    },
];

/// The payload a row hands the function: the marker the handler forwards, plus whatever else
/// the row's own shape carries.
fn payload(row: &Row) -> Value {
    let mut document = json!({ "marker": row.marker });
    match row.addressed {
        Addressed::Arn => document["detail"] = json!({ "nested": { "depth": 2 } }),
        Addressed::PartialArn => document["items"] = json!([1, 2, 3]),
        Addressed::Name | Addressed::Absent | Addressed::Whitespace | Addressed::Unpatterned => {}
    }
    document
}

/// Every message the queue `url` holds by this look, each as its own `MessageId` and body.
///
/// Read through the one `ReceiveMessage` wire this pair of suites has
/// ([`dead_lettered`]): both looks below are the same read of a queue, and a second copy of
/// it would let the two readings drift. A received message becomes visible again once its
/// visibility timeout lapses, so the loop keys what it has seen by `MessageId` — the id
/// Amazon SQS mints per message — and one message counts once however many looks return it.
async fn held(router: &MultiServiceRouter, url: &str) -> Vec<(String, Value)> {
    dead_lettered(router, url)
        .await
        .into_iter()
        .map(|message| {
            let id = message["MessageId"].as_str().unwrap_or_default().to_string();
            (id, message)
        })
        .collect()
}

/// The body of each message `held` answered, which for the sink queue is the marker the
/// handler forwarded.
fn bodies(messages: Vec<(String, Value)>) -> Vec<(String, String)> {
    messages
        .into_iter()
        .map(|(id, message)| {
            (
                id,
                message["Body"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Deploy the stack that declares the function and the role it runs as, and answer once
/// Lambda reports the function ready to be invoked (`FunctionConfiguration$State`: "Active"
/// once "the function is ready to be invoked").
///
/// Both waits are the CONDITION, looked at until this suite's own generous deadline
/// ([`PATIENCE`]): the span a stack takes to settle and a function takes to come up are
/// those services' own, so nothing here sleeps a fixed span on their behalf and no budget
/// tight enough for a loaded host to decide the outcome is shared with the rows below.
async fn given_function(router: &MultiServiceRouter) {
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", FUNCTION_STACK),
            ("TemplateBody", &function_stack()),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({FUNCTION_STACK:?}) must be accepted, got {status} with {answered}"
    );

    let began = Instant::now();
    let mut reported = stack_status(router, FUNCTION_STACK).await;
    while reported.0 != CREATE_COMPLETE {
        assert!(
            began.elapsed() < PATIENCE,
            "setup: the function's stack must reach {CREATE_COMPLETE}, last saw {:?} with {}",
            reported.0,
            reported.1
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
        reported = stack_status(router, FUNCTION_STACK).await;
    }

    let mut state = String::new();
    while state != FUNCTION_ACTIVE {
        assert!(
            began.elapsed() < PATIENCE,
            "setup: the function {FUNCTION:?} must reach {FUNCTION_ACTIVE}, last reported \
             {state:?}"
        );
        let answered = call(
            router,
            "lambda",
            &format!("lambda.{REGION}.amazonaws.com"),
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            "application/json",
            "",
        )
        .await;
        let configuration: Value =
            ::serde_json::from_str(&answered.body).unwrap_or(Value::Null);
        state = configuration["State"].as_str().unwrap_or_default().to_string();
        if state != FUNCTION_ACTIVE {
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    }
}

/// The value a dead-letter message carries under the attribute `name`.
fn attribute(message: &Value, name: &str) -> String {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Every row: the occurrence makes the `Invoke` call the universal target names, so a request
/// the operation serves RUNS the function — its own side-effect being the row's marker on the
/// sink queue — and a request it refuses runs nothing, its failed event reaching the queue the
/// schedule names under the code that operation itself answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_universal_target_invokes_the_function_its_input_names() {
    let router = build_router();
    let (sink_url, _sink_arn) = given_queue(&router, SINK_QUEUE).await;
    let (dead_letter_url, dead_letter_arn) = given_queue(&router, DEAD_LETTER_QUEUE).await;

    given_function(&router).await;

    // One execution role for every row, allowed the invocation on every function and the write
    // of a failed event: what each row reads is the ANSWER the invoked operation gave its
    // request, so no row may be stopped short by the role's own permissions — which is the
    // sibling suite's subject rather than this one's.
    let role = given_execution_role(
        &router,
        "every row",
        "scheduler-universal-lambda-role",
        &[
            ("lambda:InvokeFunction", "*"),
            ("sqs:SendMessage", dead_letter_arn.as_str()),
        ],
    )
    .await;

    // What each row's schedule asks for, kept so the assertions below read the request the
    // invocation was given rather than rebuilding it.
    let mut requests: BTreeMap<&str, String> = BTreeMap::new();
    for (index, row) in ROWS.iter().enumerate() {
        // A universal target's `Input` IS the invoked operation's own request: `Invoke` takes
        // the function in `FunctionName` and the event as `Payload`, whose value is the text
        // the function is handed.
        let input = json!({
            "FunctionName": row.addressed.function_name(FUNCTION),
            "Payload": payload(row).to_string(),
        })
        .to_string();
        given_schedule(
            &router,
            row.case,
            &format!("universal-lambda-schedule-{index}"),
            UNIVERSAL_TARGET,
            &role,
            &dead_letter_arn,
            &input,
        )
        .await;
        requests.insert(row.case, input);
    }

    let served: Vec<&Row> = ROWS.iter().filter(|row| row.refusal.is_none()).collect();
    let refused: Vec<&Row> = ROWS.iter().filter(|row| row.refusal.is_some()).collect();
    // Keyed by `MessageId`, so a message a later look sees again is the same one message.
    let mut markers: BTreeMap<String, String> = BTreeMap::new();
    let mut failures: BTreeMap<String, Value> = BTreeMap::new();
    let began = Instant::now();
    loop {
        markers.extend(bodies(held(&router, &sink_url).await));
        if failures.len() < refused.len() {
            failures.extend(held(&router, &dead_letter_url).await);
        }
        if markers.len() >= served.len() && failures.len() >= refused.len() {
            break;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "the universal target did not reach the invoked operation for every schedule: \
             expected the markers {:?} and {} failed event(s), got {:?} and {:?}",
            served.iter().map(|row| row.marker).collect::<Vec<&str>>(),
            refused.len(),
            markers.values().collect::<Vec<&String>>(),
            failures.values().collect::<Vec<&Value>>()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    let delivered: Vec<&String> = markers.values().collect();
    let reported: Vec<&Value> = failures.values().collect();

    // Each served row's marker arrived exactly once: the function RAN with the payload the
    // row's `Input` carried, read where a client reads it — the queue the handler wrote to.
    for row in &served {
        let found = delivered.iter().filter(|body| **body == row.marker).count();
        assert_eq!(
            found, 1,
            "case {:?}: expected exactly one invocation carrying the marker {:?}, got \
             {delivered:?}",
            row.case, row.marker
        );
    }
    assert_eq!(
        delivered.len(),
        served.len(),
        "expected one invocation per served schedule and none for a refused one, got \
         {delivered:?}"
    );

    // Every refused row ran nothing, and its failed event is the request the invocation made,
    // reported under the code the invoked operation answered and naming the target it was for.
    assert_eq!(
        reported.len(),
        refused.len(),
        "expected one failed event per refused call, got {reported:?}"
    );
    for row in &refused {
        let request = requests
            .get(row.case)
            .map(String::as_str)
            .expect("every row's request was kept");
        let carried: Vec<(String, String)> = reported
            .iter()
            .filter(|message| message["Body"].as_str() == Some(request))
            .map(|message| {
                (
                    attribute(message, "ERROR_CODE"),
                    attribute(message, "TARGET_ARN"),
                )
            })
            .collect();
        assert_eq!(
            carried,
            vec![(
                row.refusal.unwrap_or_default().to_string(),
                UNIVERSAL_TARGET.to_string()
            )],
            "case {:?}: expected exactly one failed event for the request {request}, under the \
             code the invoked operation answered and naming the target, got {reported:?}",
            row.case
        );
    }
}
