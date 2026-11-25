// SPDX-License-Identifier: CC0-1.0

//! What a CLIENT who DEPLOYS a least-privilege Step Functions stack gets out of it: a
//! `Task` state whose work the execution role the TEMPLATE declared does not allow, driven
//! across the five services the question spans — CloudFormation creates the role, its
//! policy and the state machine together, IAM holds the documents the call is judged
//! against, Step Functions interprets the definition and makes the call, and Amazon SQS and
//! Lambda are the services the call would have reached — mounted on ONE engine exactly as
//! `core/router/src/lib.rs` mounts them.
//!
//! RULE: a `Task` state's integration call is made acting as the state machine's `roleArn`
//! and is decided against the identity-based documents in force on that role, WHOEVER wrote
//! them — so a role and policy a template declared decide the call exactly as one a client
//! wrote through IAM's own calls would. A call those documents do not allow is NOT made:
//! the task fails with the invoked service's own access-denied refusal, naming the session
//! the call was attempted as, the action as that service words it and the ARN of the
//! resource the request addressed; the execution reaches the terminal `FAILED` carrying that
//! `error` and `cause`, and the resource the call would have acted on is untouched. A call
//! they DO allow is made, and what the task reports is the target service's own answer.
//!
//! Five crates have to agree for that to hold, so the suite lives in this target: a
//! template that declares `AWS::IAM::Role` beside `AWS::StepFunctions::StateMachine` has to
//! leave IAM holding the policy it declared, has to hand Step Functions the ARN of the role
//! it created, and the gate in front of each integration has to read that role — a
//! crate-local suite sees at most one side of it.
//!
//! The rows differ along what the rule generalises over: which service the `Task` calls and
//! therefore which refusal wording, code and status reach the execution (Amazon SQS, whose
//! front door words its own `AccessDenied` as a Forbidden; Lambda, a Forbidden under the
//! AWS-common code; and Step Functions itself, whose front door answers the same refusal as
//! a client error), and whether the policy the template declared COVERS the action the task
//! performs — which is the whole difference between a deployment that works and one that
//! only appears to.
//!
//! What the neighbours pin and this one does not repeat:
//! [`crate::sfn_task_target_authorization`] asks which RESOURCE an access request is over,
//! [`crate::sfn_execution_role_documents_in_force`] which DOCUMENTS are in force on a role a
//! client wrote through IAM, and [`crate::cloudformation_sfn_absent_role_and_stop`] what an
//! execution whose declared role cannot be ASSUMED fails with. None of them deploys the role
//! and the machine from one template, which is how a client reaches a least-privilege state
//! machine, and so none of them shows that the policy a template declared is the one the task
//! is judged by. The wire callers, the readers and the refusal spellings are
//! [`crate::sfn_task_target_authorization`]'s and [`crate::query_wire`]'s: `tests/all/main.rs`
//! is one crate root, so only the CloudFormation caller and the templates are new here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of};
use crate::sfn_task_target_authorization::{
    allow, definition, logs_only, masked, queue, ran, refusal_metadata, session_principal, sfn,
    sqs, string, waiting, ACCOUNT, FORBIDDEN, REFUSAL_CODE, REGION, SQS_REFUSAL_CODE,
};

/// The status a service whose front door answers every client error as a Bad Request
/// carries for the same refusal — Step Functions' own answer to an unauthorized call.
const CLIENT_ERROR: u16 = 400;

/// How long a row waits for a status the emulator owes: a stack reaching its terminal
/// `StackStatus`, an execution leaving `RUNNING`. Every span here is sub-second, so this is
/// slack for a loaded host rather than a budget any row is measured against.
const PATIENCE: Duration = Duration::from_secs(30);

/// How often a row looks again while it waits.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The `StackStatus` a stack reaches once the resources it declares exist in their own
/// services.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The acknowledgement a template that names the IAM role it declares has to carry: "if you
/// have IAM resources with custom names, you must specify CAPABILITY_NAMED_IAM"
/// (`CreateStack$Capabilities`).
const NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";

/// The logical ids the templates below declare their two resources under, and the outputs
/// they publish the created handles as.
const ROLE_LOGICAL_ID: &str = "TaskRole";
const MACHINE_LOGICAL_ID: &str = "Machine";
const ROLE_OUTPUT: &str = "RoleArn";
const MACHINE_OUTPUT: &str = "MachineArn";

/// The message body the queue rows' task sends, which is what a served call leaves on the
/// queue.
const BODY: &str = "hello";

/// The error a `Task` state reports when Lambda itself refuses the invocation because the
/// function does not exist (`Invoke`'s modelled `ResourceNotFoundException`), which is what
/// shows the call really reached Lambda rather than being stopped by the authorization gate.
const LAMBDA_NOT_FOUND: &str = "Lambda.ResourceNotFoundException";

/// The sentence fragment every access-denied refusal opens with, which a GRANTED row must
/// never see.
const NOT_AUTHORIZED: &str = "is not authorized to perform";

/// The five services this question spans, over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(engine),
    );
    router
}

/// One CloudFormation call over its awsQuery wire, answered with the status and the XML
/// body. The signature is never checked: this router runs with request authentication off.
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
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/cloudformation/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Deploy `template` as `stack_name` and answer the stack's `Outputs` once it has reached
/// [`CREATE_COMPLETE`], which is when the resources it declares exist in their own services.
async fn deployed(
    router: &MultiServiceRouter,
    stack_name: &str,
    template: &str,
    case: &str,
) -> BTreeMap<String, String> {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", template),
            // "If you have IAM resources with custom names, you must specify
            // CAPABILITY_NAMED_IAM" (`CreateStack$Capabilities`) — every template below
            // names the role it declares, so the client acknowledges that here.
            ("Capabilities.member.1", NAMED_IAM),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
    );

    let began = Instant::now();
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {described}"
        );
        match opt_element(&described, "StackStatus").as_deref() {
            Some(CREATE_COMPLETE) => return outputs_of(&described),
            reached => assert!(
                reached.is_none_or(|status| status.ends_with("_IN_PROGRESS")),
                "case {case:?}: expected {CREATE_COMPLETE}, got {reached:?} in {described}"
            ),
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected {CREATE_COMPLETE} within {PATIENCE:?}, got {described}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The value a stack published under `key`.
fn output(outputs: &BTreeMap<String, String>, key: &str, case: &str) -> String {
    outputs
        .get(key)
        .unwrap_or_else(|| panic!("case {case:?}: expected the stack to publish {key}: {outputs:?}"))
        .clone()
}

/// The template a row deploys: one `AWS::IAM::Role` trusted by this service and carrying
/// `document` as its one declared policy, and one `AWS::StepFunctions::StateMachine` whose
/// `RoleArn` is the ARN of that very role, with the one-`Task` definition under test.
///
/// Both handles are published as outputs — the role's ARN through `Fn::GetAtt`, the
/// machine's through `Ref`, which "returns the ARN of the state machine" — so a row reads
/// the created resources through the stack rather than guessing what it named them.
fn template(name: &str, document: &Value, definition: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE_LOGICAL_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": name,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "states.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{ "PolicyName": "task", "PolicyDocument": document }],
                },
            },
            MACHINE_LOGICAL_ID: {
                "Type": "AWS::StepFunctions::StateMachine",
                "Properties": {
                    "StateMachineName": name,
                    "RoleArn": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] },
                    "DefinitionString": definition,
                },
            },
        },
        "Outputs": {
            ROLE_OUTPUT: { "Value": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] } },
            MACHINE_OUTPUT: { "Value": { "Ref": MACHINE_LOGICAL_ID } },
        },
    })
    .to_string()
}

/// The bodies of the messages waiting on the queue at `url`, which is what a served send
/// leaves behind: the COUNT alone would not say whose payload arrived.
async fn bodies(router: &MultiServiceRouter, url: &str) -> Vec<String> {
    let received = sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    received["Messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .map(|message| string(message, "Body"))
                .collect()
        })
        .unwrap_or_default()
}

/// How many executions `ListExecutions` reports for `machine_arn`, which is how a row reads
/// whether a child the task would have started really ran.
async fn started(router: &MultiServiceRouter, machine_arn: &str) -> usize {
    let listed = sfn(
        router,
        "ListExecutions",
        &json!({ "stateMachineArn": machine_arn, "maxResults": 10 }),
    )
    .await;
    listed
        .get("executions")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// Which service a row's `Task` calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Called {
    /// Amazon SQS, through the optimized `sqs:sendMessage` integration: a served call leaves
    /// a message on the queue.
    Queue,
    /// Lambda, through the optimized `lambda:invoke` integration. The function named was
    /// never created, so a served call comes back as Lambda's own
    /// [`LAMBDA_NOT_FOUND`] — which is what distinguishes a call that was MADE from one the
    /// gate stopped.
    Function,
    /// Step Functions itself, through the optimized `states:startExecution` integration: a
    /// served call leaves an execution on the child machine.
    Child,
}

/// What each called service's answer to an unauthorized call is: the `Resource` token the
/// `Task` names, the action a policy GRANTS it under, the action the refusal names (as that
/// service words one), the error a `Task` reports it under, the code its answer carries, the
/// name its own SDK client gives it, and the status it answers with.
struct Answer {
    resource: &'static str,
    grant_action: &'static str,
    refused_action: &'static str,
    refusal_error: &'static str,
    code: &'static str,
    sdk_client: &'static str,
    status: u16,
}

impl Called {
    fn answer(self) -> Answer {
        match self {
            // Amazon SQS's own front door answers an unauthorized call as a Forbidden
            // carrying `AccessDenied` — not the AWS-common code — and names the action it
            // refused in the lower case its authorization layer reports.
            Self::Queue => Answer {
                resource: "arn:aws:states:::sqs:sendMessage",
                grant_action: "sqs:SendMessage",
                refused_action: "sqs:sendmessage",
                refusal_error: "SQS.AmazonSQSException",
                code: SQS_REFUSAL_CODE,
                sdk_client: "AmazonSQS",
                status: FORBIDDEN,
            },
            // An invocation is subject to the permission Lambda PUBLISHES for `Invoke`
            // (`lambda:InvokeFunction`), which is not the operation's own name, and `Invoke`
            // models no error for an unauthorized call, so the refusal arrives under
            // Lambda's base SDK exception.
            Self::Function => Answer {
                resource: "arn:aws:states:::lambda:invoke",
                grant_action: "lambda:InvokeFunction",
                refused_action: "lambda:InvokeFunction",
                refusal_error: "Lambda.AWSLambdaException",
                code: REFUSAL_CODE,
                // That integration is served by the current AWS SDK, which names the
                // service as its own client does.
                sdk_client: "Lambda",
                status: FORBIDDEN,
            },
            // Step Functions' own front door answers an authorization failure as a client
            // error, so the same refusal reaches the state under a different status.
            Self::Child => Answer {
                resource: "arn:aws:states:::states:startExecution",
                grant_action: "states:StartExecution",
                refused_action: "states:StartExecution",
                refusal_error: "StepFunctions.AWSStepFunctionsException",
                code: REFUSAL_CODE,
                sdk_client: "AWSStepFunctions",
                status: CLIENT_ERROR,
            },
        }
    }
}

/// One row: which service the `Task` calls, and whether the policy the template declares
/// covers the action that call is subject to.
struct Row {
    case: &'static str,
    called: Called,
    granted: bool,
}

/// The rows, in the order they are driven.
const ROWS: &[Row] = &[
    Row {
        case: "a send whose declared policy grants it",
        called: Called::Queue,
        granted: true,
    },
    Row {
        case: "the same send, deployed under a policy that grants nothing of it",
        called: Called::Queue,
        granted: false,
    },
    Row {
        case: "an invocation whose declared policy grants it",
        called: Called::Function,
        granted: true,
    },
    Row {
        case: "the same invocation, deployed under a policy that grants nothing of it",
        called: Called::Function,
        granted: false,
    },
    Row {
        case: "a child execution whose declared policy grants it",
        called: Called::Child,
        granted: true,
    },
    Row {
        case: "the same child execution, deployed under a policy that grants nothing of it",
        called: Called::Child,
        granted: false,
    },
];

/// A `Task` state of a CloudFormation-deployed state machine is made only where the policy
/// the template declared for its execution role allows the call, and a call it does not
/// allow terminates the execution `FAILED` with the invoked service's own refusal while the
/// target is left untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deployed_machines_task_is_judged_by_the_role_its_template_declared() {
    let router = emulator();
    // A function no `CreateFunction` was ever made for: a GRANTED invocation of it is
    // answered by Lambda itself, which is what shows the call was made, and a refused one
    // never reaches Lambda at all.
    let function_arn = format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:absent-handler");

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let answer = row.called.answer();

        // The target the row's task addresses, and the ARN a policy names it by. Each row
        // gets its own, so what one row's task did is never what the next row reads.
        let (queue_url, target_arn, child_arn) = match row.called {
            Called::Queue => {
                let (url, arn) = queue(&router, &format!("least-privilege-{index}")).await;
                (Some(url), arn, None)
            }
            Called::Function => (None, function_arn.clone(), None),
            Called::Child => {
                let created = sfn(
                    &router,
                    "CreateStateMachine",
                    &json!({
                        "name": format!("least-privilege-child-{index}"),
                        "definition": r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","End":true}}}"#,
                        // The child's own states do no work, so nothing is ever done as
                        // this role: what the row varies is the PARENT's.
                        "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/unused-{index}"),
                    }),
                )
                .await;
                let arn = string(&created, "stateMachineArn");
                (None, arn.clone(), Some(arn))
            }
        };

        let parameters = match row.called {
            Called::Queue => json!({
                "QueueUrl": queue_url.clone().expect("a queue row addresses a queue"),
                "MessageBody": BODY,
            }),
            Called::Function => json!({ "FunctionName": target_arn.clone(), "Payload": {} }),
            Called::Child => json!({ "StateMachineArn": target_arn.clone() }),
        };

        // The policy the TEMPLATE declares: the log writes a state machine makes whatever
        // else it does, plus — for a granted row — the grant for this call over the ARN of
        // what the request addresses.
        let mut statements = vec![logs_only()];
        if row.granted {
            statements.push(allow(answer.grant_action, &target_arn));
        }
        let document = json!({ "Version": "2012-10-17", "Statement": statements });

        let name = format!("least-privilege-{index}");
        let outputs = deployed(
            &router,
            &format!("least-privilege-stack-{index}"),
            &template(
                &name,
                &document,
                &definition(answer.resource, &parameters),
            ),
            case,
        )
        .await;
        let role_arn = output(&outputs, ROLE_OUTPUT, case);
        let machine_arn = output(&outputs, MACHINE_OUTPUT, case);

        // The machine the stack created really carries the role the stack created: the
        // template handed Step Functions the ARN of its own `AWS::IAM::Role`, which is what
        // every assertion below is about.
        let described_machine = sfn(
            &router,
            "DescribeStateMachine",
            &json!({ "stateMachineArn": machine_arn }),
        )
        .await;
        assert_eq!(
            string(&described_machine, "roleArn"),
            role_arn,
            "case {case:?}: expected the deployed machine to carry the deployed role \
             {role_arn:?}, got {described_machine}"
        );

        let (_execution_arn, described) = ran(&router, &machine_arn, case).await;

        if row.granted {
            match row.called {
                Called::Queue => {
                    assert_eq!(
                        described.get("status").and_then(Value::as_str),
                        Some("SUCCEEDED"),
                        "case {case:?}: expected the granted send to be made, got {described}"
                    );
                    let url = queue_url.clone().expect("a queue row addresses a queue");
                    let waiting = bodies(&router, &url).await;
                    assert_eq!(
                        waiting,
                        vec![BODY.to_string()],
                        "case {case:?}: expected the queue to carry the body the task sent, got \
                         {waiting:?}"
                    );
                }
                Called::Child => {
                    assert_eq!(
                        described.get("status").and_then(Value::as_str),
                        Some("SUCCEEDED"),
                        "case {case:?}: expected the granted start to be made, got {described}"
                    );
                    let child = child_arn.clone().expect("a child row addresses a machine");
                    assert_eq!(
                        started(&router, &child).await,
                        1,
                        "case {case:?}: expected the execution the task started on the child"
                    );
                }
                Called::Function => {
                    // The call was made, so what the task reports is Lambda's own answer
                    // about the function it looked for — not an authorization refusal.
                    assert_eq!(
                        (
                            described.get("status").and_then(Value::as_str),
                            described.get("error").and_then(Value::as_str),
                        ),
                        (Some("FAILED"), Some(LAMBDA_NOT_FOUND)),
                        "case {case:?}: expected Lambda's own answer to the granted call, got \
                         {described}"
                    );
                    let cause = string(&described, "cause");
                    assert!(
                        !cause.contains(NOT_AUTHORIZED),
                        "case {case:?}: expected no access denial once the declared policy \
                         grants the call, got {cause:?}"
                    );
                    assert!(
                        cause.contains(&target_arn),
                        "case {case:?}: expected Lambda's answer to name the function it looked \
                         for {target_arn:?}, got {cause:?}"
                    );
                }
            }
            continue;
        }

        // The refused side: a terminal FAILED carrying the invoked service's own refusal.
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(answer.refusal_error)),
            "case {case:?}: expected FAILED with {:?}, got {described}",
            answer.refusal_error
        );

        // The sentence, as that service words it: the SESSION the call was attempted as —
        // read back out of the cause, because the session's name is the assume's own — then
        // the action and the ARN of the resource the request addressed, and WHY, which for
        // a declared policy that says nothing about the call is that nothing allows it.
        let cause = string(&described, "cause");
        let principal = session_principal(&role_arn, &cause);
        let opens = format!(
            "User: {principal} is not authorized to perform: {} on resource: {target_arn} \
             because no identity-based policy allows the {} action ",
            answer.refused_action, answer.refused_action
        );
        assert!(
            cause.starts_with(&opens),
            "case {case:?}: expected the cause to open {opens:?}, got {cause:?}"
        );
        // And the metadata the client this integration is served by closes the message
        // with: the invoked service as that client names it, the status it answers an
        // unauthorized call with, and the code its answer named the class by.
        let closes = refusal_metadata(answer.resource, answer.sdk_client, answer.code, answer.status);
        let reported = masked(&cause);
        assert!(
            reported.ends_with(&closes),
            "case {case:?}: expected the cause to close {closes:?}, got {reported:?}"
        );
        // The role's own IAM ARN names a principal rather than the session the call was made
        // as, so it appears nowhere in the sentence.
        assert!(
            !cause.contains(&role_arn),
            "case {case:?}: expected the cause never to name the role ARN {role_arn:?}, got \
             {cause:?}"
        );

        // And the call was never made: the target the deployment would have reached is
        // untouched.
        match row.called {
            Called::Queue => {
                let url = queue_url.clone().expect("a queue row addresses a queue");
                assert_eq!(
                    waiting(&router, &url).await,
                    0,
                    "case {case:?}: expected the queue the declared role may not send to to be \
                     untouched"
                );
            }
            Called::Child => {
                let child = child_arn.clone().expect("a child row addresses a machine");
                assert_eq!(
                    started(&router, &child).await,
                    0,
                    "case {case:?}: expected no execution of the child the declared role may \
                     not start"
                );
            }
            // Nothing of Lambda's own is reachable for a function that was never created;
            // that the refusal is the GATE's and not Lambda's is what the error name above
            // says, since a call that reached Lambda answers [`LAMBDA_NOT_FOUND`].
            Called::Function => {}
        }
    }
}
