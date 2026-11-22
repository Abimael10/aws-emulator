// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack records for an `AWS::Pipes::Pipe` its owning service accepts and
//! then settles in a terminal failure state.
//!
//! RULE: a resource whose PROVIDER reports that it did not stabilize fails inside the one
//! envelope CloudFormation quotes a provider's answer in — `Resource handler returned message:
//! "<the provider's report>" (RequestToken: <token>, HandlerErrorCode: <class>)` — and the class
//! is the provider's OWN classification of what it reported: a provider that gives up on a
//! resource with its own account of a resource that did not stabilize reports it under
//! `NotStabilized`, not under the class for a service that refused or failed a call. For this
//! type the report names the resource TYPE and the identifier it was asked about (the pipe's
//! name) and then states EventBridge Pipes' own `StateReason` after the join.
//!
//! Why it matters: `HandlerErrorCode` is the machine-readable half of a failed deployment — the
//! field deployment tooling branches on to tell a resource that never came up from a request the
//! owning service would not take — and CI that points an operator at the failing resource parses
//! the `did not stabilize` report out of the envelope. A class of the wrong kind reads as a
//! service-side fault on a declaration nothing was wrong with.
//!
//! The rows differ along what the rule generalises over: WHY EventBridge Pipes could not bring
//! the declared pipe up — the execution role is granted none of the calls its source stage makes
//! on the source (stated nowhere, or granted on another queue), and the `Source` names a queue
//! Amazon SQS does not hold — which changes the inner `StateReason` and nothing of the envelope
//! or the class. The POSITIVE CONTROL is the same template with a role that may make every call
//! over a source the stack itself creates: that stack completes, its pipe's resource records no
//! failure at all, and EventBridge Pipes reports the pipe `RUNNING`.
//!
//! Three crates have to agree for this to hold — CloudFormation drives the deployment and the
//! rollback, EventBridge Pipes decides that the pipe cannot come up and says why, IAM holds the
//! role and Amazon SQS holds the queues — so the suite lives here rather than in any one of them.
//! `core/router/tests/all/cloudformation_pipe_update_rollback_restores_the_working_role.rs` is
//! the UPDATE path of the same relation, and
//! `services/service-com-amazonaws-pipes-pipes/tests/all/pipe_bring_up.rs` is the table of what
//! that service reports.
//!
//! Every wait here looks again until the condition it needs holds, on a generous deadline;
//! nothing is asserted about elapsed time.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// The Region every row deploys in.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// How long a stack operation is given to reach a terminal status, and how long the pipe of the
/// control row is given to come to rest. Well above the spans those moves take, so a row fails
/// only when nothing ever settled.
const PATIENCE: Duration = Duration::from_secs(60);

/// How often a stack or a pipe is looked at again while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The logical id every row declares its pipe under.
const PIPE: &str = "ThePipe";

/// The resource type the provider's report names, which is the type the template declares.
const PIPE_TYPE: &str = "AWS::Pipes::Pipe";

/// The statuses this suite reads off a stack and off a resource event.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const CREATE_FAILED: &str = "CREATE_FAILED";

/// What the state EventBridge Pipes reports a pipe that is moving records in is called.
const RUNNING: &str = "RUNNING";

/// The `HandlerErrorCode` class a provider reports a resource THAT DID NOT STABILIZE under — a
/// value of the CloudFormation model's own `HandlerErrorCode` enum.
const NOT_STABILIZED: &str = "NotStabilized";

/// Where the envelope's quoted message ends: the token that follows is one run's own value, so
/// what a row asserts is the message before it, that a token follows, and the class it closes
/// with.
const BEFORE_THE_TOKEN: &str = "\" (RequestToken: ";

/// The head of the envelope, which the quoted message must not carry a second time.
const ENVELOPE_OPENS: &str = "Resource handler returned message: \"";

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
    router.register_service(service_com_amazonaws_pipes_pipes::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which is
/// what routes the request. The signature is never checked: this router runs with request
/// authentication off.
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
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// `DescribePipe` over EventBridge Pipes' own `restJson1` wire, on the URI that operation's
/// `@http` binding declares (`GET /v1/pipes/{Name}`).
async fn describe_pipe(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/v1/pipes/{name}"))
        .header("host", format!("pipes.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("pipes"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The stack as it reads once its operation has reached a TERMINAL status, polled rather than
/// slept for: how many spans the pipe's own moves take is the deployment's to decide.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks answered {status} {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") || Instant::now() >= deadline {
            return described;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every `<member>` block of the stack's events, walking the pages the operation hands back.
///
/// Bounded by the pages a row's stack can have: a page carries a token only while there are more
/// events, and the walk stops when one carries none or when it has taken more pages than any
/// row's stack produces.
async fn stack_events(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<String> {
    const PAGE_BUDGET: usize = 32;
    let mut events: Vec<String> = Vec::new();
    let mut token = String::new();
    for _ in 0..PAGE_BUDGET {
        let mut members: Vec<(&str, &str)> = vec![("StackName", stack)];
        if !token.is_empty() {
            members.push(("NextToken", token.as_str()));
        }
        let (status, page) = cloudformation(router, "DescribeStackEvents", &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStackEvents answered {status} {page}"
        );
        events.extend(members_of(&page).into_iter().map(ToString::to_string));
        match opt_element(&page, "NextToken") {
            Some(next) if !next.is_empty() => token = next,
            _ => return events,
        }
    }
    panic!("case {case:?}: the stack's events did not end within {PAGE_BUDGET} pages")
}

/// The `ResourceStatusReason` of every event that reports `logical_id` in `status`.
async fn reasons_for(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    status: &str,
) -> Vec<String> {
    stack_events(router, case, stack)
        .await
        .iter()
        .filter(|event| {
            elements(event, "LogicalResourceId")
                .first()
                .is_some_and(|reported| reported == logical_id)
                && elements(event, "ResourceStatus")
                    .first()
                    .is_some_and(|reported| reported == status)
        })
        .map(|event| {
            elements(event, "ResourceStatusReason")
                .first()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

/// The trust policy every declared role carries: EventBridge Pipes acts as the pipe's `RoleArn`,
/// so a role a stack creates for a pipe has to admit that service principal.
fn pipes_trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "pipes.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The ARN of the queue called `name`: the same total function of the Region, the account and the
/// name Amazon SQS mints one from, so a statement naming it names the very resource the pipe's
/// call is made on — and a `Source` written this way for a queue nobody created is well formed
/// and names nothing.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
}

/// What EventBridge Pipes reports as the reason a pipe whose execution role may not take its
/// Amazon SQS source's records is not coming up, and the reason it reports when that same call
/// could not be CARRIED OUT at all, because the source is a queue that service does not hold.
///
/// Each is the fixed text of its failure class, naming the call and the service it is made to and
/// nothing of the request.
/// `services/service-com-amazonaws-pipes-pipes/tests/all/pipe_bring_up.rs` is each sentence's own
/// table, over the source kinds that decide which call is named; they are written out again here
/// because what this suite asserts is the text CloudFormation QUOTES.
const WITHHELD_SOURCE_READ: &str = "Input parameter is invalid from the request due to : The \
                                    provided customer role does not have permissions to call \
                                    ReceiveMessage on SQS";
const FAILED_SOURCE_READ: &str = "Input parameter is invalid from the request due to : Error \
                                  occurs while ReceiveMessage on SQS";

/// How a row's declared pipe comes to be one EventBridge Pipes cannot bring up.
#[derive(Clone, Copy)]
enum CannotComeUp {
    /// Its `Source` is the source queue the template creates, and the role it acts as is granted
    /// none of the calls its source stage makes on that queue.
    WithheldRole,
    /// Its role may make every call, and its `Source` names a queue Amazon SQS does not hold.
    AbsentSource,
}

/// One row: a declaration the owning service accepts and then cannot bring up, and the
/// `StateReason` it therefore reports — which is what the provider's report states after its
/// join.
struct Row {
    case: &'static str,
    /// The prefix every name this row declares carries, so two rows never share a resource.
    prefix: &'static str,
    cannot_come_up: CannotComeUp,
    /// The statements of the role the pipe acts as, built from the row's own queue names: what
    /// the rule turns on is the (action, RESOURCE) pair the call that takes the source's records
    /// is made on.
    statements: fn(&Row) -> Value,
    /// The sentence EventBridge Pipes reports for the pipe, which the provider's report quotes.
    state_reason: &'static str,
}

impl Row {
    fn stack(&self) -> String {
        format!("{}-stack", self.prefix)
    }

    fn source(&self) -> String {
        format!("{}-source", self.prefix)
    }

    fn target(&self) -> String {
        format!("{}-target", self.prefix)
    }

    fn pipe(&self) -> String {
        format!("{}-pipe", self.prefix)
    }

    /// The queue the row's pipe is pointed at: the one the template creates, or — for the row
    /// whose source is absent — a well-formed ARN of this account and Region naming a queue
    /// nothing created.
    fn declared_source(&self) -> Value {
        match self.cannot_come_up {
            CannotComeUp::WithheldRole => json!({ "Fn::GetAtt": ["SourceQueue", "Arn"] }),
            CannotComeUp::AbsentSource => json!(queue_arn(&format!("{}-ghost", self.prefix))),
        }
    }

    /// The template this row deploys: the two queues, the role the pipe acts as, and the pipe.
    fn template(&self) -> String {
        json!({
            "Resources": {
                "SourceQueue": {
                    "Type": "AWS::SQS::Queue",
                    "Properties": { "QueueName": self.source() },
                },
                "TargetQueue": {
                    "Type": "AWS::SQS::Queue",
                    "Properties": { "QueueName": self.target() },
                },
                "PipeRole": {
                    "Type": "AWS::IAM::Role",
                    "Properties": {
                        "AssumeRolePolicyDocument": pipes_trust_policy(),
                        "Policies": [{
                            "PolicyName": "move-records",
                            "PolicyDocument": {
                                "Version": "2012-10-17",
                                "Statement": (self.statements)(self),
                            },
                        }],
                    },
                },
                PIPE: {
                    "Type": PIPE_TYPE,
                    "Properties": {
                        "Name": self.pipe(),
                        "Source": self.declared_source(),
                        "Target": { "Fn::GetAtt": ["TargetQueue", "Arn"] },
                        "RoleArn": { "Fn::GetAtt": ["PipeRole", "Arn"] },
                        "SourceParameters": { "SqsQueueParameters": { "BatchSize": 1 } },
                        "DesiredState": RUNNING,
                    },
                },
            },
            "Outputs": {
                "PipeName": { "Value": { "Ref": PIPE } },
            },
        })
        .to_string()
    }
}

/// The statements a role that may make every call on everything carries: the control's role, and
/// the one the absent-source row acts as, so what fails that row is its source and nothing else.
fn every_call(_row: &Row) -> Value {
    json!([{
        "Effect": "Allow",
        "Action": [
            "sqs:ReceiveMessage",
            "sqs:DeleteMessage",
            "sqs:GetQueueAttributes",
            "sqs:SendMessage",
        ],
        "Resource": "*",
    }])
}

/// The rows, differing in why EventBridge Pipes cannot bring the declared pipe up.
const ROWS: &[Row] = &[
    Row {
        case: "a role that states nothing at all about the pipe's source",
        prefix: "pipe-class-unstated",
        cannot_come_up: CannotComeUp::WithheldRole,
        statements: |_| {
            json!([{ "Effect": "Allow", "Action": ["sqs:SendMessage"], "Resource": "*" }])
        },
        state_reason: WITHHELD_SOURCE_READ,
    },
    Row {
        case: "a role whose source-stage grants name ANOTHER queue than the pipe's source",
        prefix: "pipe-class-other",
        cannot_come_up: CannotComeUp::WithheldRole,
        statements: |row| {
            json!([{
                "Effect": "Allow",
                "Action": ["sqs:*"],
                "Resource": queue_arn(&row.target()),
            }])
        },
        state_reason: WITHHELD_SOURCE_READ,
    },
    Row {
        case: "a Source naming a queue Amazon SQS does not hold",
        prefix: "pipe-class-ghost",
        cannot_come_up: CannotComeUp::AbsentSource,
        statements: every_call,
        state_reason: FAILED_SOURCE_READ,
    },
];

/// The whole message CloudFormation quotes for a pipe its owning service settled in a failure
/// state: the provider's report that the resource did not stabilize, naming the type it was asked
/// for and the identifier it was asked about, with that service's own reason after the join.
fn did_not_stabilize(identifier: &str, state_reason: &str) -> String {
    format!(
        "Resource of type '{PIPE_TYPE}' with identifier '{identifier}' did not stabilize. Status \
         Reason is {state_reason}"
    )
}

/// Create the row's stack and poll it to the status it settles in, answering that read's body.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) -> String {
    let (status, created) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack answered {status} {created}"
    );
    settled_stack(router, case, stack).await
}

/// RULE, every row: the stack rolls back, the pipe's resource records exactly one `CREATE_FAILED`
/// event, and its reason is the provider's did-not-stabilize report — quoting EventBridge Pipes'
/// own `StateReason` — inside the handler envelope, closing with the class a resource that did
/// not stabilize is reported under.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pipe_that_cannot_stabilise_fails_as_not_stabilized() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let stack = row.stack();
        let described = deploy(&router, case, &stack, &row.template()).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack of a pipe that cannot come up to roll back to \
             {ROLLBACK_COMPLETE}, got {reported:?} with the pipe's failures {:?}",
            reasons_for(&router, case, &stack, PIPE, CREATE_FAILED).await
        );

        let failures = reasons_for(&router, case, &stack, PIPE, CREATE_FAILED).await;
        assert_eq!(
            failures.len(),
            1,
            "case {case:?}: expected exactly one {CREATE_FAILED} event for {PIPE}, got {failures:?}"
        );
        let reason = failures.first().cloned().unwrap_or_default();
        let (head, tail) = reason
            .split_once(BEFORE_THE_TOKEN)
            .unwrap_or_else(|| panic!("case {case:?}: no request token in {reason:?}"));
        let quoted = head.strip_prefix(ENVELOPE_OPENS).unwrap_or_else(|| {
            panic!("case {case:?}: expected the provider envelope, got {reason:?}")
        });
        assert_eq!(
            quoted,
            did_not_stabilize(&row.pipe(), row.state_reason),
            "case {case:?}: expected the provider's did-not-stabilize report, got {quoted:?}"
        );
        assert!(
            !quoted.contains(ENVELOPE_OPENS),
            "case {case:?}: expected the provider's answer to be quoted ONCE, got {quoted:?}"
        );
        let (token, class) = tail
            .trim_end_matches(')')
            .split_once(", HandlerErrorCode: ")
            .unwrap_or_else(|| panic!("case {case:?}: no HandlerErrorCode in {reason:?}"));
        assert!(
            !token.is_empty(),
            "case {case:?}: expected the resource request's own token, got {reason:?}"
        );
        assert_eq!(
            class, NOT_STABILIZED,
            "case {case:?}: expected a resource that did not stabilize to be classified \
             {NOT_STABILIZED}, got {reason:?}"
        );
    }
}

/// The CONTROL: the same template with a role that may make every call, over the source queue the
/// stack itself creates. The stack completes, the pipe's resource records no failure at all, and
/// EventBridge Pipes reports the pipe running — so what the rows above pin is a provider reporting
/// a failure rather than a template nothing can deploy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pipe_that_can_come_up_records_no_failure() {
    let router = build_router();
    let case = "a role that may make every call over the source the stack creates";
    let row = Row {
        case,
        prefix: "pipe-class-control",
        cannot_come_up: CannotComeUp::WithheldRole,
        statements: every_call,
        state_reason: WITHHELD_SOURCE_READ,
    };
    let stack = row.stack();
    let described = deploy(&router, case, &stack, &row.template()).await;
    let reported = opt_element(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: expected the stack to complete, got {reported:?} in {described}"
    );
    let failures = reasons_for(&router, case, &stack, PIPE, CREATE_FAILED).await;
    assert!(
        failures.is_empty(),
        "case {case:?}: expected the pipe's resource to record no failure, got {failures:?}"
    );

    // The pipe the stack published is a pipe EventBridge Pipes holds, by the name the stack
    // published for it, and it is in the state a pipe that is moving records rests in.
    let published = outputs_of(&described)
        .get("PipeName")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no PipeName output in {described}"));
    assert_eq!(
        published,
        row.pipe(),
        "case {case:?}: expected the stack to publish the declared pipe name, got {published:?}"
    );
    let deadline = Instant::now() + PATIENCE;
    let state = loop {
        let (status, read) = describe_pipe(&router, &published).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribePipe {published} answered {status} {read}"
        );
        let state = read["CurrentState"].as_str().unwrap_or_default().to_string();
        if state == RUNNING || Instant::now() >= deadline {
            break state;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        state, RUNNING,
        "case {case:?}: expected the deployed pipe to be {RUNNING}, it reports {state:?}"
    );
}
