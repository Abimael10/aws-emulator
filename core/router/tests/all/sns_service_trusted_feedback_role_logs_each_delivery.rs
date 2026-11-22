// SPDX-License-Identifier: CC0-1.0

//! A role whose trust policy admits the Amazon SNS service principal really is a role that
//! service ACTS AS, and a client can see it: the topic naming it as a delivery-status feedback
//! role accepts the configuration, the publish is delivered to every subscription of that topic
//! — the subscribed function runs and the subscribed queue holds the notification — and one
//! delivery-status record per delivery lands in CloudWatch Logs, written as that role.
//!
//! The rule pinned here: **a service can assume a role exactly when the trust document IAM
//! holds for that role grants the service's own principal `sts:AssumeRole` FOR the work being
//! done — the assume of a topic's feedback role is made for one topic and carries the
//! confused-deputy keys of that work (`aws:SourceArn` naming the topic, `aws:SourceAccount`
//! naming its owner), so a statement's `Condition` is part of its grant — and a service that
//! can act as the role performs the work the role authorizes: Amazon SNS assumes the topic's
//! `<Protocol>SuccessFeedbackRoleArn` and writes one record per delivery over that protocol
//! into `sns/<region>/<account-id>/<topic-name>` ("The <ENDPOINT>SuccessFeedbackRoleArn and
//! <ENDPOINT>FailureFeedbackRoleArn attributes are used to give Amazon SNS write access to use
//! CloudWatch Logs on your behalf", `SetTopicAttributesInput$AttributeName`), while a role it
//! cannot act as is refused at the write and leaves no record. The DELIVERY itself is never
//! conditional on that role — the record reports a delivery that has already been attempted —
//! so every subscription of the topic is delivered to whatever the trust document says.**
//!
//! That is what makes a correctly trusted service principal distinguishable from an untrusted
//! one for a client that only ever observes the far end: both publishes arrive, and only the
//! trusted one leaves the record its role authorized.
//!
//! Six crates have to agree for it to hold — IAM holds the role and the trust document that
//! decides the assume, Amazon SNS reads that document and fans the publish out, Lambda admits
//! the invocation and RUNS the handler, Amazon SQS holds both the notification the topic
//! delivered and what the handler wrote, CloudWatch Logs holds the records, and CloudFormation
//! archives the inline handler into the function's deployment package — so the suite lives
//! here: no single service crate's test target can configure the role and then read the log.
//!
//! The rows differ along what the rule generalises over: what the trust document says about
//! this service's assume FOR this topic — the principal named alone, named among several and
//! narrowed by `Condition` to this very topic, another service's principal instead, and a role
//! ARN this account's IAM holds nothing under (no evidence about a role provisioned out of band,
//! so not refused, and nothing to assume either). Every row exercises BOTH protocols the topic
//! is subscribed over, whose feedback attributes carry different prefixes, and asserts both
//! halves — the two deliveries and the records — so a delivery that never happened and a record
//! nothing authorized are told apart.
//!
//! The remaining document classes the same rule covers — an explicit deny beside the allow, a
//! grant narrowed to another topic, an action wildcard, the account root, another STS action —
//! are the row set of the SNS crate's own
//! `tests/all/feedback_role_trust.rs`, which reads the write's half of the rule over all ten
//! feedback attributes; the two tables are to be kept in step. What cannot be read there, and is
//! what this suite adds, is the DELIVERY and the record at the far end: that target mounts
//! neither Lambda, nor Amazon SQS, nor CloudWatch Logs.
//!
//! The router, the awsQuery and Amazon SQS callers, the execution role and the forwarding
//! handler are the sibling SNS -> Lambda suite's
//! ([`crate::sns_lambda_subscription_permission_boundary`]), so there is one harness for these
//! readings rather than a copy per reading; what this file owns is the trust documents, the
//! feedback-role writes and the log reads.
//!
//! The CloudWatch Logs caller and the record reader are this file's own rather than
//! [`crate::cloudformation_sns_delivery_status_logging`]'s, which reads the same records off a
//! STACK-declared topic: that suite signs every call for another Region, and the Region is in
//! the host header its callers write, in the ARNs its resources carry and in the log group's
//! own name — so its readers cannot see a topic of the Region this suite's shared harness signs
//! for. What this file's reader adds beyond it is the filtering by the published message's id,
//! which is what tells one row's records from the next's.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{decoded_entries, element, form_encode};

use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, drain, execution_role, iam, lambda, sqs, ACCOUNT, CREATE_COMPLETE,
    FORWARDING_SOURCE, LOOK_INTERVAL, ROLE_NAME,
};

/// The Region every request here is signed for: the one the shared helpers and the shared stack
/// waiter sign for, so the resources reached through them are the ones this suite creates. It
/// is also the Region the topic's ARN carries, and so the one in its log group's name.
const REGION: &str = "us-east-1";

/// The service principal a delivery-status feedback role has to admit for Amazon SNS to assume
/// it, and the one a subscribed queue's policy grants the send to.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// Another service's principal, which a row names instead to show that it is THIS service's
/// principal and not merely a service principal that decides the assume.
const OTHER_PRINCIPAL: &str = "events.amazonaws.com";

/// The action a trust document grants a principal that may assume the role.
const ASSUME_ACTION: &str = "sts:AssumeRole";

/// The condition key naming the RESOURCE a cross-service assume is made for — the topic whose
/// delivery-status records the role is assumed to write.
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The CloudWatch Logs actions a delivery-status record costs the assumed role: the group and
/// the stream are created on first write and then the record is put. The names are CloudWatch
/// Logs' own operations (`smithy_query(query="list_operations")` of the `logs` model:
/// `CreateLogGroup`, `CreateLogStream`, `PutLogEvents`). Every role a row creates is granted
/// them, so the only thing the rows differ in is the TRUST.
const LOG_WRITE_ACTIONS: [&str; 3] = [
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// The percentage of successful deliveries each row asks to have logged: all of them, so a
/// record that is owed is owed for every delivery rather than for a sampled share of them.
const FULL_SAMPLE: &str = "100";

/// The two protocols every row's topic is subscribed over, each with the prefix Amazon SNS
/// spells that protocol's delivery-status attributes with — source: the protocol prefixes
/// `SetTopicAttributesInput$AttributeName` documents (`HTTP`, `Firehose`, `Lambda`,
/// `Application`, `SQS`) against the `SubscribeInput$Protocol` values they belong to.
const PROTOCOLS: [(&str, &str); 2] = [("lambda", "Lambda"), ("sqs", "SQS")];

/// The stack that holds the forwarding function and the sink it writes into, and the names it
/// gives them.
const STACK: &str = "sns-trusted-feedback";
const SINK_QUEUE: &str = "sns-trusted-feedback-sink";
const FORWARDER: &str = "sns-trusted-feedback-forwarder";

/// The statement id the function's policy admits this service's invocation under.
const INVOKE_STATEMENT: &str = "sns-invoke";

/// How long an observation that must SEE something keeps looking for it: a function coming up,
/// an asynchronous invocation, a handler's own write, a record a delivery owed.
///
/// Deliberately generous. Every wait below leaves as soon as what it waits for is there, so this
/// bound decides only how long a LOADED machine is given — it is not a span any assertion
/// depends on, and the work it covers is other services' own, performed in-process alongside
/// every other test of this target.
const OBSERVATION_BUDGET: Duration = Duration::from_secs(60);

/// How long a row that must observe NOTHING keeps looking before concluding nothing came — well
/// past the span the rows that DO observe a record take, which is one write's worth of work
/// after a delivery that has already been seen.
const QUIET_BUDGET: Duration = Duration::from_secs(3);

/// How long every row waits after what it expects is first there, before the count is asserted:
/// a record or a delivery that happened twice lands inside it.
const SETTLE_SLACK: Duration = Duration::from_secs(1);

/// The whole emulator as it is ASSEMBLED, on one engine: the calls Amazon SNS makes to IAM,
/// Lambda, Amazon SQS and CloudWatch Logs are dispatched back into this same router, so the
/// records and the notifications this suite reads are the ones those deliveries wrote.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. Neither the key id nor the signature is a credential: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One Amazon SNS awsQuery call, with whatever status it answered: a row whose feedback-role
/// write must be REFUSED reads that refusal here rather than asserting a 200.
async fn sns_query(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("sns"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Amazon SNS call that must be answered 200.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = sns_query(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SNS {action} must be answered 200, got {status} with {body}"
    );
    body
}

/// One CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`), with whatever status it
/// answered: a group no record has created yet is a refusal this suite reads THROUGH rather than
/// asserts on.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header("authorization", authorization("logs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The stack this suite deploys: the forwarding function alone, under the execution role IAM
/// already holds and carrying the handler that writes into the sink. A stack is what archives an
/// inline handler into a function's deployment package, which is why the function comes from one;
/// everything else — the sink, the topic, the feedback role and the two subscriptions — is made
/// through the APIs a client makes them through.
fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Forwarder": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FORWARDER,
                    "Role": format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Timeout": 10,
                    "Code": { "ZipFile": FORWARDING_SOURCE.replace("SINK_NAME", SINK_QUEUE) },
                },
            },
        },
    })
    .to_string()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active` — the state
/// an invocation needs — and answer the state it reported then, with its ARN. The span a created
/// function spends coming up is that service's own, so nothing here sleeps a fixed span for it.
async fn active_function(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let deadline = Instant::now() + OBSERVATION_BUDGET;
    loop {
        let (_, reported) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        let state = reported["State"].as_str().unwrap_or_default().to_string();
        let arn = reported["FunctionArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state == "Active" || Instant::now() >= deadline {
            return (state, arn);
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The policy AWS documents for a queue subscribed to an SNS topic, which is what makes the
/// delivery to it succeed.
fn queue_grants_sns(queue_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": { "Service": SNS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        }],
    })
    .to_string()
}

/// A queue called `name`: its url and the ARN a subscription names it by. `grants` asks for the
/// policy that lets this service send to it, which a queue a topic delivers into needs and the
/// sink the handler writes into — with the handler's own execution role — does not.
///
/// The visibility window is set past the whole suite, so a body one row received cannot come
/// back visible and be counted again in a later one.
async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    grants: bool,
) -> (String, String) {
    let created = sqs(
        router,
        "CreateQueue",
        &json!({ "QueueName": name, "Attributes": { "VisibilityTimeout": "300" } }),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue answered no QueueUrl: {created}"))
        .to_string();
    let described = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = described["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {described}"))
        .to_string();
    if grants {
        sqs(
            router,
            "SetQueueAttributes",
            &json!({ "QueueUrl": url, "Attributes": { "Policy": queue_grants_sns(&arn) } }),
        )
        .await;
    }
    (url, arn)
}

/// The identity policy every feedback role a row creates carries: the CloudWatch Logs write one
/// record costs.
fn log_write_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": LOG_WRITE_ACTIONS,
            "Resource": "*",
        }],
    })
    .to_string()
}

/// The sentence Amazon SNS states for a value it cannot use as a delivery-status logging role.
fn not_a_valid_logging_role(name: &str, value: &str) -> String {
    format!(
        "Invalid parameter: {name}: {value} is not a valid role to allow SNS to write to \
         Cloudwatch Logs"
    )
}

/// This service's principal, named alone.
fn names_this_service(_topic: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": SNS_PRINCIPAL },
            "Action": ASSUME_ACTION,
        }],
    })
}

/// This service's principal among several, with the assume narrowed to the very topic the
/// feedback role is configured on — the scoping a logging role is routinely provisioned with.
fn names_several_for_this_topic(topic: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": [OTHER_PRINCIPAL, SNS_PRINCIPAL] },
            "Action": ASSUME_ACTION,
            "Condition": { "StringEquals": { SOURCE_ARN_KEY: topic } },
        }],
    })
}

/// Another service's principal instead of this one's.
fn names_another_service(_topic: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": OTHER_PRINCIPAL },
            "Action": ASSUME_ACTION,
        }],
    })
}

/// One row: the trust document IAM holds for the row's feedback role, and what must come of
/// naming that role on the topic and publishing to it.
struct Row {
    case: &'static str,
    /// What distinguishes this row's topic, role and queue from the others'.
    slug: &'static str,
    /// The trust document, built from the topic the assume would be made for; `None` for a row
    /// where IAM holds no role under the ARN at all.
    trust: Option<fn(&str) -> Value>,
    /// Whether the write of the feedback-role attributes must be accepted and read back.
    accepted: bool,
    /// Whether the deliveries must leave their delivery-status records.
    logged: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the trust document names this service's principal alone",
        slug: "named",
        trust: Some(names_this_service),
        accepted: true,
        logged: true,
    },
    Row {
        case: "it names this service among several, for this very topic",
        slug: "scoped",
        trust: Some(names_several_for_this_topic),
        accepted: true,
        logged: true,
    },
    Row {
        case: "it names another service's principal instead",
        slug: "other",
        trust: Some(names_another_service),
        accepted: false,
        logged: false,
    },
    Row {
        // The two halves of the rule read the same ARN under different burdens of proof, which
        // is why this row is accepted and still logs nothing. A role IAM holds nothing under is
        // no evidence ABOUT a role the account may hold out of band, so the write has nothing to
        // refuse on; the record, on the other hand, is written only by actually acting as the
        // role, and there is no document granting that. A client sees the configuration it asked
        // for and no records under it.
        case: "this account's IAM holds no role under the ARN at all",
        slug: "unheld",
        trust: None,
        accepted: true,
        logged: false,
    },
];

/// Every delivery-status record `log_group` holds about the message `message_id`, across every
/// stream in it — and an empty list for a group that is not there at all, which is what a
/// configuration nothing could be assumed for leaves behind.
async fn records_of(
    router: &MultiServiceRouter,
    log_group: &str,
    message_id: &str,
) -> Vec<Value> {
    let (status, streams) = logs(
        router,
        "DescribeLogStreams",
        &json!({ "logGroupName": log_group }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    let mut records: Vec<Value> = Vec::new();
    for stream in streams["logStreams"].as_array().cloned().unwrap_or_default() {
        let Some(name) = stream["logStreamName"].as_str() else {
            continue;
        };
        let (status, events) = logs(
            router,
            "GetLogEvents",
            &json!({
                "logGroupName": log_group,
                "logStreamName": name,
                "startFromHead": true,
            }),
        )
        .await;
        if status != 200 {
            continue;
        }
        records.extend(
            events["events"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|event| event["message"].as_str())
                .filter_map(|message| serde_json::from_str::<Value>(message).ok())
                .filter(|record| record["notification"]["messageId"] == json!(message_id)),
        );
    }
    records
}

/// Look for `wanted` records of `message_id` until they are there or the budget is spent, then
/// look once more past a settling span so a record written twice is counted too. A row expecting
/// NONE looks for the whole quiet window, so an empty group is not a group read too early.
async fn records_within(
    router: &MultiServiceRouter,
    log_group: &str,
    message_id: &str,
    wanted: usize,
) -> Vec<Value> {
    let budget = if wanted > 0 {
        OBSERVATION_BUDGET
    } else {
        QUIET_BUDGET
    };
    let deadline = Instant::now() + budget;
    loop {
        let records = records_of(router, log_group, message_id).await;
        if (wanted > 0 && records.len() >= wanted) || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    tokio::time::sleep(SETTLE_SLACK).await;
    records_of(router, log_group, message_id).await
}

/// Wait for the subscribed function to have forwarded `marker` into the sink, accumulating every
/// body the sink ever handed out: a forward of an earlier row's publish is still in `seen` for
/// this row's reading to skip over.
async fn forwarded(
    router: &MultiServiceRouter,
    sink: &str,
    seen: &mut Vec<String>,
    marker: &str,
) -> usize {
    let deadline = Instant::now() + OBSERVATION_BUDGET;
    loop {
        seen.extend(drain(router, sink).await);
        let held = seen.iter().filter(|body| body.as_str() == marker).count();
        if held > 0 || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Wait for the subscribed queue to hold the notification carrying `marker`, and answer the
/// messages of every notification it held.
async fn notified(router: &MultiServiceRouter, url: &str, marker: &str) -> Vec<String> {
    let deadline = Instant::now() + OBSERVATION_BUDGET;
    let mut seen: Vec<String> = Vec::new();
    loop {
        seen.extend(
            drain(router, url)
                .await
                .iter()
                .filter_map(|body| serde_json::from_str::<Value>(body).ok())
                .map(|envelope| envelope["Message"].as_str().unwrap_or_default().to_string()),
        );
        if seen.iter().any(|message| message.as_str() == marker) || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Every row: the trust document decides whether the feedback role is taken and whether its
/// records are written, and never whether the publish is delivered.
#[tokio::test]
async fn an_sns_trusted_feedback_role_is_assumed_and_logs_every_delivery_it_authorizes() {
    let router = build_router();

    // The execution role the template names, and the sink the deployed handler writes into.
    execution_role(&router).await;
    let (sink, _) = given_queue(&router, "setup", SINK_QUEUE, false).await;
    cloudformation(
        &router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template())],
    )
    .await;
    let (status, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "setup: the stack holding the subscribed function must deploy, got {status} with \
         {described}"
    );

    let (state, function_arn) = active_function(&router, FORWARDER).await;
    assert_eq!(
        state, "Active",
        "setup: expected Lambda to report {FORWARDER} Active, got {state:?}"
    );

    // The invocation a delivery makes is admitted by the FUNCTION's own policy, and this one is
    // granted unscoped: every row publishes from a topic of its own, and which topic is not what
    // these rows are about.
    let (granted, answered) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{FORWARDER}/policy"),
        Some(json!({
            "StatementId": INVOKE_STATEMENT,
            "Action": "lambda:InvokeFunction",
            "Principal": SNS_PRINCIPAL,
        })),
    )
    .await;
    assert_eq!(
        granted, 201,
        "setup: expected HTTP 201 from AddPermission, got {granted} with {answered}"
    );

    let mut seen: Vec<String> = Vec::new();

    for row in ROWS {
        let case = row.case;
        let topic_name = format!("sns-trusted-feedback-{}", row.slug);
        let created = sns(&router, case, "CreateTopic", &[("Name", &topic_name)]).await;
        let topic = element(&created, "TopicArn");
        assert!(
            !topic.is_empty(),
            "case {case:?}: CreateTopic must answer a TopicArn, got {created}"
        );

        // The role the topic will name, under the trust document this row is about. A row with
        // no document leaves IAM holding nothing under the ARN.
        let role_name = format!("sns-trusted-feedback-{}-role", row.slug);
        let role_arn = format!("arn:aws:iam::{ACCOUNT}:role/{role_name}");
        if let Some(trust) = row.trust {
            let document = trust(&topic).to_string();
            let made = iam(
                &router,
                "CreateRole",
                &[
                    ("RoleName", &role_name),
                    ("AssumeRolePolicyDocument", &document),
                ],
            )
            .await;
            assert_eq!(
                element(&made, "Arn"),
                role_arn,
                "case {case:?}: IAM must hold the role under the ARN the topic will name, got \
                 {made}"
            );
            iam(
                &router,
                "PutRolePolicy",
                &[
                    ("RoleName", &role_name),
                    ("PolicyName", "write-the-delivery-status-records"),
                    ("PolicyDocument", &log_write_policy()),
                ],
            )
            .await;
        }

        // The two subscriptions: the function, and a queue of this row's own that grants this
        // service the send.
        let (queue_url, queue_arn) = given_queue(
            &router,
            case,
            &format!("sns-trusted-feedback-{}-queue", row.slug),
            true,
        )
        .await;
        // Each protocol of the catalog above, paired with the endpoint subscribed over it, by
        // zipping rather than by index: a third protocol row stays correctly wired.
        let endpoints = [function_arn.as_str(), queue_arn.as_str()];
        for ((protocol, _), endpoint) in PROTOCOLS.into_iter().zip(endpoints) {
            let subscribed = sns(
                &router,
                case,
                "Subscribe",
                &[
                    ("TopicArn", &topic),
                    ("Protocol", protocol),
                    ("Endpoint", endpoint),
                    ("ReturnSubscriptionArn", "true"),
                ],
            )
            .await;
            let arn = element(&subscribed, "SubscriptionArn");
            assert!(
                arn.starts_with(&topic),
                "case {case:?}: subscribing {endpoint} over {protocol} must answer a confirmed \
                 subscription of the topic, got {subscribed}"
            );
        }

        // The sample rate is no role, so it lands whatever the trust document says; the ROLE
        // attributes are what this row is about.
        for (_, prefix) in PROTOCOLS {
            sns(
                &router,
                case,
                "SetTopicAttributes",
                &[
                    ("TopicArn", &topic),
                    ("AttributeName", &format!("{prefix}SuccessFeedbackSampleRate")),
                    ("AttributeValue", FULL_SAMPLE),
                ],
            )
            .await;
        }
        for (_, prefix) in PROTOCOLS {
            let name = format!("{prefix}SuccessFeedbackRoleArn");
            let (status, body) = sns_query(
                &router,
                "SetTopicAttributes",
                &[
                    ("TopicArn", &topic),
                    ("AttributeName", &name),
                    ("AttributeValue", &role_arn),
                ],
            )
            .await;
            if row.accepted {
                assert_eq!(
                    status, 200,
                    "case {case:?}: writing {name} must be accepted, got {status} with {body}"
                );
            } else {
                assert_eq!(
                    status, 400,
                    "case {case:?}: writing {name} must be refused, got {status} with {body}"
                );
                assert_eq!(
                    element(&body, "Code"),
                    "InvalidParameter",
                    "case {case:?}: expected the InvalidParameter code, got {body}"
                );
                assert_eq!(
                    element(&body, "Message"),
                    not_a_valid_logging_role(&name, &role_arn),
                    "case {case:?}: expected this service's own sentence, got {body}"
                );
            }
        }

        // What the topic CARRIES is what the delivery is logged by, so it is read back rather
        // than assumed from the writes above.
        let read = sns(
            &router,
            case,
            "GetTopicAttributes",
            &[("TopicArn", &topic)],
        )
        .await;
        let attributes = decoded_entries(&read);
        for (_, prefix) in PROTOCOLS {
            let name = format!("{prefix}SuccessFeedbackRoleArn");
            let held = attributes.get(&name).cloned();
            let expected = row.accepted.then(|| role_arn.clone());
            assert_eq!(
                held, expected,
                "case {case:?}: expected the topic to carry {name} = {expected:?}, got {held:?}"
            );
        }

        let marker = format!("marker-{}", row.slug);
        let published = sns(
            &router,
            case,
            "Publish",
            &[("TopicArn", &topic), ("Message", &marker)],
        )
        .await;
        let message_id = element(&published, "MessageId");
        assert!(
            !message_id.is_empty(),
            "case {case:?}: Publish must answer a MessageId, got {published}"
        );

        // Both deliveries happen whatever the trust document says: the record reports a delivery
        // that has already been attempted, so the role decides the record and never the delivery.
        let forwards = forwarded(&router, &sink, &mut seen, &marker).await;

        assert_eq!(
            forwards, 1,
            "case {case:?}: expected the subscribed function to be invoked once and to forward \
             {marker:?} to the sink, got {forwards} (every body seen so far: {seen:?})"
        );
        let messages = notified(&router, &queue_url, &marker).await;
        assert_eq!(
            messages,
            vec![marker.clone()],
            "case {case:?}: expected the subscribed queue to hold exactly the published \
             notification, got {messages:?}"
        );

        // The records: one per delivery when the role could be assumed for this topic, none at
        // all otherwise — the group itself is never created then.
        let log_group = format!("sns/{REGION}/{ACCOUNT}/{topic_name}");
        let wanted = if row.logged { PROTOCOLS.len() } else { 0 };
        let records = records_within(&router, &log_group, &message_id, wanted).await;
        assert_eq!(
            records.len(),
            wanted,
            "case {case:?}: expected {wanted} delivery-status record(s) in {log_group}, got \
             {records:?}"
        );
        let destinations: BTreeSet<String> = records
            .iter()
            .filter_map(|record| record["delivery"]["destination"].as_str())
            .map(str::to_string)
            .collect();
        let expected: BTreeSet<String> = if row.logged {
            [function_arn.clone(), queue_arn.clone()].into_iter().collect()
        } else {
            BTreeSet::new()
        };
        assert_eq!(
            destinations, expected,
            "case {case:?}: expected one record per subscribed endpoint, got {records:?}"
        );
        for record in &records {
            assert_eq!(
                record["status"].as_str(),
                Some("SUCCESS"),
                "case {case:?}: expected the record of a delivery the endpoint took to report \
                 SUCCESS, got {record}"
            );
            assert_eq!(
                record["notification"]["topicArn"].as_str(),
                Some(topic.as_str()),
                "case {case:?}: the record must name the topic it was published to, got {record}"
            );
        }
    }
}
