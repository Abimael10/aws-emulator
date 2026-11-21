// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack UPDATE does when it switches a working `AWS::Pipes::Pipe` onto
//! an execution role that cannot read the pipe's source.
//!
//! RULE: a resource whose owning service ACCEPTS the change and then settles the resource in a
//! terminal failure state did not STABILISE, so the update of that resource fails rather than
//! completing — the stack records exactly ONE `UPDATE_FAILED` event for its logical id, whose
//! `ResourceStatusReason` is the provider's own report of a resource that did not stabilize
//! (naming the resource TYPE and the identifier it was asked about) quoting the reason that
//! service gave, inside the one envelope CloudFormation quotes a provider's answer in — and
//! the stack is then ROLLED BACK to `UPDATE_ROLLBACK_COMPLETE`, which puts the declaration
//! that was in force before the update back into the owning service. For a pipe that means the
//! role it was working under: the rolled-back pipe is on its previous `RoleArn`, EventBridge
//! Pipes reports it `RUNNING`, and it goes on moving records.
//!
//! Why it matters: a client that gates a deployment on the stack's status is otherwise told a
//! broken deployment succeeded, and is left with a pipe that silently moves nothing while both
//! CloudFormation and `DescribePipe` report it healthy.
//!
//! Four crates have to agree for this to hold — CloudFormation drives the update and the
//! rollback, EventBridge Pipes decides that the pipe cannot come up on the new role, IAM holds
//! both roles, and Amazon SQS holds the source and the target — so the suite lives here rather
//! than in any one of them.
//! `core/router/tests/all/cloudformation_deployed_pipe_delivery.rs` is the CREATE side of the
//! same relation (a pipe that could never poll its source rolls its stack back), and
//! `services/service-com-amazonaws-pipes-pipes/tests/all/pipe_bring_up.rs` is the table of what
//! that service reports.
//!
//! The rows differ along what the rule generalises over: HOW the new role withholds the call
//! that takes the source's records — policies stating nothing about it, a grant naming another
//! queue, an explicit `Deny` over a wildcard allow — each of which leaves the same report.
//!
//! Every wait here looks again until the condition it needs holds, on a generous deadline;
//! nothing is asserted about elapsed time.

use std::collections::BTreeSet;
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

/// How long a stack operation is given to reach a terminal status. Well above the spans the
/// pipe's own moves take, so a row fails only when the operation never settled.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long a delivery the pipe has to make is given before a row fails.
const BUDGET: Duration = Duration::from_secs(30);

/// How often a stack, a queue or a pipe is looked at again while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// `TargetParameters.InputTemplate`, so each delivered message carries the source record's own
/// body and a delivery can be attributed to the record that caused it.
const BODY_TEMPLATE: &str = "<$.body>";

/// The logical id every row declares its pipe under.
const PIPE: &str = "ThePipe";

/// The resource type the provider's report names, which is the type the template declares.
const PIPE_TYPE: &str = "AWS::Pipes::Pipe";

/// The statuses this suite reads off a stack and off a resource event.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";
const UPDATE_FAILED: &str = "UPDATE_FAILED";

/// What the state EventBridge Pipes reports a pipe that is moving records in is called.
const RUNNING: &str = "RUNNING";

/// Where the envelope's quoted message ends and the request's own token begins. The token is
/// one run's own value, so what a row asserts is the message before it and that a token
/// follows.
const BEFORE_THE_TOKEN: &str = " (RequestToken: ";

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

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request. The signature is never checked: this router runs with request
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

/// One Amazon SQS call over its `awsJson1.0` wire.
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
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
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
    let began = Instant::now();
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks answered {status} {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") || began.elapsed() >= PATIENCE {
            return described;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every `<member>` block of the stack's events, walking the pages the operation hands back.
///
/// Bounded by the pages a row's stack can have: a page carries a token only while there are
/// more events, and the walk stops when one carries none or when it has taken more pages than
/// any row's stack produces.
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

/// Send `body` to the queue at `url`.
async fn send(router: &MultiServiceRouter, case: &str, url: &str, body: &str) {
    let (status, answered) = sqs(
        router,
        "SendMessage",
        &json!({ "QueueUrl": url, "MessageBody": body }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SendMessage answered {status} {answered}"
    );
}

/// Every message body seen at `url` until `wanted` has been seen among them, or the budget
/// lapses. `VisibilityTimeout: 0` takes no checkout, so nothing read here is hidden from a
/// later look.
async fn collected_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: &str,
) -> BTreeSet<String> {
    let deadline = Instant::now() + BUDGET;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    loop {
        let (status, answered) = sqs(
            router,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "VisibilityTimeout": 0 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ReceiveMessage answered {status} {answered}"
        );
        seen.extend(
            answered["Messages"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|message| message["Body"].as_str().unwrap_or_default().to_string()),
        );
        if seen.contains(wanted) || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The pipe as EventBridge Pipes reports it once it has come to rest on the role the rollback
/// put back, polled rather than slept for: the rollback's own `UpdatePipe` is a move of its
/// own, and how long it takes is that service's to decide.
async fn pipe_resting_on(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
) -> Value {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, reported) = describe_pipe(router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribePipe {name} answered {status} {reported}"
        );
        let settled = reported["RoleArn"].as_str() == Some(role_arn)
            && reported["CurrentState"].as_str() == Some(RUNNING);
        if settled || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The trust policy both roles carry: EventBridge Pipes acts as the pipe's `RoleArn`, so a role
/// a stack creates for a pipe has to admit that service principal.
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

/// A role declaration carrying `statements` as its one inline policy.
fn role(statements: Value) -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "AssumeRolePolicyDocument": pipes_trust_policy(),
            "Policies": [{
                "PolicyName": "move-records",
                "PolicyDocument": { "Version": "2012-10-17", "Statement": statements },
            }],
        },
    })
}

/// The statements the WORKING role carries: the calls an Amazon SQS source's stage makes on the
/// source, and the one an Amazon SQS target's stage makes on the target.
fn admits_every_call() -> Value {
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

/// One row: how the role the update switches the pipe onto withholds the source read.
struct Row {
    case: &'static str,
    /// The prefix every name this row declares carries, so two rows never share a resource.
    prefix: &'static str,
    /// The statements of the role the update points the pipe at — built from the row's own
    /// queue names, because what the rule turns on is the (action, RESOURCE) pair the call that
    /// takes the source's records is made on.
    withheld: fn(&Row) -> Value,
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

    /// The ARN of the queue called `name`: the same total function of the Region, the account
    /// and the name Amazon SQS mints one from, so a statement here names the very resource the
    /// pipe's call is made on.
    fn queue_arn(name: &str) -> String {
        format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
    }

    /// The template this row deploys, with the pipe's `RoleArn` reading the role whose logical
    /// id is `pipe_role`. Both roles are declared either way, so the ONE thing an update
    /// changes is which of them the pipe acts as.
    fn template(&self, pipe_role: &str) -> String {
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
                "GoodRole": role(admits_every_call()),
                "BadRole": role((self.withheld)(self)),
                PIPE: {
                    "Type": PIPE_TYPE,
                    "Properties": {
                        "Name": self.pipe(),
                        "Source": { "Fn::GetAtt": ["SourceQueue", "Arn"] },
                        "Target": { "Fn::GetAtt": ["TargetQueue", "Arn"] },
                        "RoleArn": { "Fn::GetAtt": [pipe_role, "Arn"] },
                        "SourceParameters": { "SqsQueueParameters": { "BatchSize": 1 } },
                        "TargetParameters": { "InputTemplate": BODY_TEMPLATE },
                    },
                },
            },
            "Outputs": {
                "SourceUrl": { "Value": { "Ref": "SourceQueue" } },
                "TargetUrl": { "Value": { "Ref": "TargetQueue" } },
                "GoodRoleArn": { "Value": { "Fn::GetAtt": ["GoodRole", "Arn"] } },
                "BadRoleArn": { "Value": { "Fn::GetAtt": ["BadRole", "Arn"] } },
            },
        })
        .to_string()
    }
}

/// The rows, differing in how the role the update switches to withholds the call that takes the
/// source's records.
const ROWS: &[Row] = &[
    Row {
        case: "a role that states nothing at all about taking the source's records",
        prefix: "pipe-update-unstated",
        withheld: |_| {
            json!([{
                "Effect": "Allow",
                "Action": ["sqs:SendMessage", "sqs:GetQueueAttributes"],
                "Resource": "*",
            }])
        },
    },
    Row {
        case: "a role whose receive grant names ANOTHER queue than the pipe's source",
        prefix: "pipe-update-other",
        withheld: |row| {
            json!([{
                "Effect": "Allow",
                "Action": ["sqs:*"],
                "Resource": Row::queue_arn(&row.target()),
            }])
        },
    },
    Row {
        case: "a role whose wildcard allow is overridden by a deny on the source receive",
        prefix: "pipe-update-deny",
        withheld: |row| {
            json!([
                { "Effect": "Allow", "Action": ["sqs:*"], "Resource": "*" },
                {
                    "Effect": "Deny",
                    "Action": ["sqs:ReceiveMessage"],
                    "Resource": Row::queue_arn(&row.source()),
                },
            ])
        },
    },
];

/// What EventBridge Pipes reports as the reason a pipe whose execution role may not take its
/// Amazon SQS source's records is not coming up: the fixed text of that failure class, naming
/// the call the role is missing.
///
/// `services/service-com-amazonaws-pipes-pipes/tests/all/pipe_bring_up.rs` is that sentence's
/// own table, over the source kinds that decide which call is named; it is written out again
/// here because what this suite asserts is the text CloudFormation QUOTES.
const PIPE_STATE_REASON: &str = "Input parameter is invalid from the request due to : The \
                                 provided customer role does not have permissions to call \
                                 ReceiveMessage on SQS";

/// The whole message CloudFormation quotes for a resource whose owning service settled it in a
/// failure state: the provider's report that the resource did not stabilize, naming the type it
/// was asked for and the identifier it was asked about, with that service's own reason after
/// the join.
fn did_not_stabilize(identifier: &str) -> String {
    format!(
        "Resource handler returned message: \"Resource of type '{PIPE_TYPE}' with identifier \
         '{identifier}' did not stabilize. Status Reason is {PIPE_STATE_REASON}\""
    )
}

/// RULE, every row: the stack is created with the pipe on the working role and really moves a
/// record; the update onto the row's role cannot stabilise, so the pipe's resource records ONE
/// `UPDATE_FAILED` event carrying the provider's did-not-stabilize report, the stack reaches
/// `UPDATE_ROLLBACK_COMPLETE`, and the rolled-back pipe is on the role it was working under,
/// `RUNNING`, and moving records again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pipe_update_that_cannot_stabilise_rolls_back_to_the_working_role() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let stack = row.stack();

        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &row.template("GoodRole")),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack answered {status} {created}"
        );
        let described = settled_stack(&router, case, &stack).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: expected the stack {CREATE_COMPLETE}, got {reported:?} in \
             {described}"
        );

        let outputs = outputs_of(&described);
        let output = |key: &str| {
            outputs
                .get(key)
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
        };
        let source_url = output("SourceUrl");
        let target_url = output("TargetUrl");
        let good_role = output("GoodRoleArn");
        let bad_role = output("BadRoleArn");
        let pipe = row.pipe();

        // The positive control: the deployed pipe is a live pipe that moves records.
        let before = format!("{}-before", row.prefix);
        send(&router, case, &source_url, &before).await;
        let seen = collected_until(&router, case, &target_url, &before).await;
        assert!(
            seen.contains(&before),
            "case {case:?}: expected the deployed pipe to deliver {before:?} before the \
             update, saw {seen:?}"
        );

        // The update: the same template with the pipe's RoleArn reading the other role.
        let (status, updated) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &row.template("BadRole")),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateStack answered {status} {updated}"
        );

        let described = settled_stack(&router, case, &stack).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, UPDATE_ROLLBACK_COMPLETE,
            "case {case:?}: expected the update the pipe could not stabilise to roll back to \
             {UPDATE_ROLLBACK_COMPLETE}, got {reported:?} with the pipe's failures {:?}",
            reasons_for(&router, case, &stack, PIPE, UPDATE_FAILED).await
        );

        // Exactly ONE event reports the pipe's resource as failed to update, and it carries the
        // provider's own report of a resource that did not stabilize.
        let failures = reasons_for(&router, case, &stack, PIPE, UPDATE_FAILED).await;
        assert_eq!(
            failures.len(),
            1,
            "case {case:?}: expected exactly one {UPDATE_FAILED} event for {PIPE}, got \
             {failures:?}"
        );
        let reason = failures.first().cloned().unwrap_or_default();
        let (quoted, token) = reason
            .split_once(BEFORE_THE_TOKEN)
            .unwrap_or_else(|| panic!("case {case:?}: no request token in {reason:?}"));
        assert_eq!(
            quoted,
            did_not_stabilize(&pipe),
            "case {case:?}: expected the provider's did-not-stabilize report, got {quoted:?}"
        );
        assert!(
            !token.is_empty(),
            "case {case:?}: expected the resource request's own token, got {reason:?}"
        );

        // The rollback put the declaration that was in force back: the pipe is on the role it
        // was working under, and that service reports it running.
        let rolled_back = pipe_resting_on(&router, case, &pipe, &good_role).await;
        assert_eq!(
            rolled_back["RoleArn"].as_str(),
            Some(good_role.as_str()),
            "case {case:?}: expected the rollback to restore the working role, the pipe reports \
             {rolled_back} (the role the update named is {bad_role})"
        );
        assert_eq!(
            rolled_back["CurrentState"].as_str(),
            Some(RUNNING),
            "case {case:?}: expected the rolled-back pipe {RUNNING}, it reports {rolled_back}"
        );

        // ...and it is a WORKING pipe again: a record sent after the rollback reaches the
        // target.
        let after = format!("{}-after", row.prefix);
        send(&router, case, &source_url, &after).await;
        let seen = collected_until(&router, case, &target_url, &after).await;
        assert!(
            seen.contains(&after),
            "case {case:?}: expected the rolled-back pipe to deliver {after:?}, saw {seen:?}"
        );
    }
}
