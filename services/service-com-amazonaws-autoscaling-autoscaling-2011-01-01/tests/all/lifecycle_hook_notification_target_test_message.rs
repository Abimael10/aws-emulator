// SPDX-License-Identifier: CC0-1.0

//! A lifecycle hook's notification target is PROVED as the hook is put: the call publishes
//! the test message to it under the hook's role, and a role that may not publish there
//! refuses the call.
//!
//! The RULE under test, in one sentence: a `PutLifecycleHook` that NAMES a notification
//! target publishes an `autoscaling:TEST_NOTIFICATION` message for the group to that target
//! under the hook's role — "when you specify a notification target, Amazon EC2 Auto Scaling
//! sends it a test message. Test messages contain the following additional key-value pair:
//! "Event": "autoscaling:TEST_NOTIFICATION""
//! (`smithy_query(query="doc",shape="PutLifecycleHookType",member="NotificationTargetARN")`),
//! the role being "the ARN of the IAM role that allows the Auto Scaling group to publish to
//! the specified notification target" (same shape, `RoleARN`) — so a pair the account's own
//! IAM answers cannot carry that message is refused synchronously with `ValidationError`
//! and stores no hook, while a pair that can makes the target RECEIVE the message.
//!
//! The rows span what the rule generalises over: which service holds the target (an Amazon
//! SQS queue, an Amazon SNS topic, a vendor this service publishes no notification to), what
//! the role's documents say (nothing at all, the action on that very target, the action on
//! every resource, every action of that service, the action on ANOTHER resource, another
//! service's action, an allow beside an explicit deny), whether IAM holds the role at all,
//! and which half of the pair the request named (both, the target alone, the role alone).
//!
//! IAM, Amazon SQS and Amazon SNS are mounted BESIDE Auto Scaling and Amazon EC2 on the SAME
//! engine, so the decision is taken against the account's real IAM records and the delivery
//! travels the real in-process callback to a real queue: nothing here reads the publisher's
//! own state for either.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam_service;

use crate::support::{
    borrowed, form_encode, iam_query, json_rpc, p, query, seed_launch_templates, tag_value,
    xml_unescape, REGION,
};

/// The event a test message carries, spelled out here rather than imported, so a wrong
/// constant in the implementation cannot make a row pass.
const TEST_NOTIFICATION: &str = "autoscaling:TEST_NOTIFICATION";

/// The `Service` every notification of this service names itself by.
const NOTIFICATION_SERVICE: &str = "AWS Auto Scaling";

/// The launch template every group here is created from.
const TEMPLATE: &str = "lt-hook-test-notification";

/// The transition every hook here is put for.
const TRANSITION: &str = "autoscaling:EC2_INSTANCE_LAUNCHING";

/// The API version an Amazon SNS awsQuery client sends.
const SNS_API_VERSION: &str = "2010-03-31";

/// How often a poll re-reads the queue a delivery is observed on.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up — generous, because what it waits for is
/// a CONDITION and not a duration: the delivery is made inside the call it is asserted of.
const POLL_BUDGET: Duration = Duration::from_secs(30);

/// The whole message a request whose role may not publish to its target is refused with:
/// fixed text around the TWO ARNs the request carried. Spelled out here rather than taken
/// from the crate, so the assertion does not take the handler's own word for the sentence.
fn unable_to_publish_message(target_arn: &str, role_arn: &str) -> String {
    format!(
        "Unable to publish test message to notification target {target_arn} using IAM role \
         {role_arn}. Please check your target and role configuration and try to put lifecycle \
         hook again."
    )
}

/// One emulator: Auto Scaling, Amazon EC2, IAM, Amazon SQS and Amazon SNS over ONE shared
/// engine — the five services the decision and the delivery travel through.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    seed_launch_templates(&engine, &[TEMPLATE]);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(iam_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// One Amazon SQS call, which must succeed.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, parsed) = json_rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SQS {operation} expected 200, got {status}: {parsed}"
    );
    parsed
}

/// One awsQuery form call against Amazon SNS, which must succeed.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={SNS_API_VERSION}");
    for (key, value) in params {
        body.push('&');
        body.push_str(key);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}/sns/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: SNS {action} expected 200, got {status}: {text}"
    );
    text
}

/// One queue of this emulator: its URL and its ARN, both as Amazon SQS reports them.
#[derive(Debug, Clone)]
struct Queue {
    url: String,
    arn: String,
}

/// Create the queue `name` through Amazon SQS's own wire.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> Queue {
    let created = sqs(router, case, "CreateQueue", json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    Queue { url, arn }
}

/// Create the topic `name` with a queue of this emulator subscribed to it, so a publish to
/// the topic is observed where a client observes it. Answers the topic's ARN and the queue.
async fn given_topic_with_subscriber(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, Queue) {
    let created = sns(router, case, "CreateTopic", &[("Name", name)]).await;
    let topic_arn = tag_value(&created, "TopicArn");
    assert!(
        !topic_arn.is_empty(),
        "case {case:?}: CreateTopic must report the topic's ARN, got {created}"
    );
    let queue = given_queue(router, case, name).await;
    // A delivery Amazon SNS is not granted is withheld by the queue, so the queue grants the
    // SNS service principal the send AWS documents for a subscribed queue.
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "sns.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": queue.arn,
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        json!({ "QueueUrl": queue.url, "Attributes": { "Policy": policy } }),
    )
    .await;
    sns(
        router,
        case,
        "Subscribe",
        &[
            ("TopicArn", &topic_arn),
            ("Protocol", "sqs"),
            ("Endpoint", &queue.arn),
        ],
    )
    .await;
    (topic_arn, queue)
}

/// Create the IAM role `role_name`, trusted by this service's own principal, carrying
/// `document` as its one inline permissions policy when it carries one at all. Answers the
/// ARN IAM ITSELF reports for it, which is the value a request names it by — read from the
/// service that owns the role rather than composed here.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    document: Option<&str>,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "autoscaling.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, body) = iam_query(
        router,
        "CreateRole",
        &[
            p("RoleName", role_name),
            p("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating the role {role_name:?} expected 200, got {status}: {body}"
    );
    let arn = tag_value(&body, "Arn");
    assert!(
        !arn.is_empty(),
        "case {case:?}: CreateRole must report the role's ARN, got {body}"
    );
    if let Some(document) = document {
        let (status, body) = iam_query(
            router,
            "PutRolePolicy",
            &[
                p("RoleName", role_name),
                p("PolicyName", "publish"),
                p("PolicyDocument", document),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: storing the inline policy of {role_name:?} expected 200, got \
             {status}: {body}"
        );
    }
    arn
}

/// The ARN of a role of this account that IAM holds NO record of: one it created and then
/// deleted, so the value is a well-formed ARN of a role of this account that resolves to
/// nothing — which is how a role provisioned out of band looks to this emulator.
async fn given_absent_role(router: &MultiServiceRouter, case: &str, role_name: &str) -> String {
    let arn = given_role(router, case, role_name, None).await;
    let (status, body) = iam_query(router, "DeleteRole", &[p("RoleName", role_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: deleting the role {role_name:?} expected 200, got {status}: {body}"
    );
    arn
}

/// One policy document allowing `action` on `resource`.
fn allow(action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
    .to_string()
}

/// One document allowing every action of a service on everything AND explicitly denying
/// `action`.
fn allow_but_deny(allowed: &str, denied: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": allowed, "Resource": "*" },
            { "Effect": "Deny", "Action": denied, "Resource": "*" },
        ],
    })
    .to_string()
}

/// Create one empty group (nothing to launch, so nothing settles on the clock while the
/// hooks are written and read).
async fn given_group(router: &MultiServiceRouter, case: &str, name: &str) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
        p("AvailabilityZones.member.1", "us-west-2a"),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating the group {name:?} expected 200, got {status}: {body}"
    );
}

/// The ARN `DescribeAutoScalingGroups` reports for the group `name`.
async fn group_arn(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    let arn = tag_value(&body, "AutoScalingGroupARN");
    assert!(
        !arn.is_empty(),
        "case {case:?}: the group {name:?} must report its ARN, got {body}"
    );
    arn
}

/// Post one `PutLifecycleHook` carrying `members` beyond the two names, and answer its
/// status, wire error code and message.
async fn put_hook(
    router: &MultiServiceRouter,
    group: &str,
    hook: &str,
    members: &[(String, String)],
) -> (u16, String, String) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("LifecycleHookName", hook),
    ];
    params.extend_from_slice(members);
    let (status, body) = query(router, "PutLifecycleHook", &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// The `NotificationTargetARN` the group's own reader reports for the hook `hook`, and `""`
/// when it reports no such hook at all.
async fn stored_target(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    hook: &str,
) -> Option<String> {
    let (status, body) = query(
        router,
        "DescribeLifecycleHooks",
        &[
            ("AutoScalingGroupName", group),
            ("LifecycleHookNames.member.1", hook),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeLifecycleHooks expected 200, got {status}: {body}"
    );
    let reported = tag_value(&body, "LifecycleHookName");
    if reported.is_empty() {
        return None;
    }
    Some(tag_value(&body, "NotificationTargetARN"))
}

/// Every message waiting on the queue right now, each read as the document Auto Scaling
/// published — out of the Amazon SNS envelope when it arrived through a topic.
async fn drain(router: &MultiServiceRouter, case: &str, queue: &Queue) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        json!({ "QueueUrl": queue.url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            let body: Value = serde_json::from_str(message["Body"].as_str().unwrap_or_default())
                .unwrap_or(Value::Null);
            // A delivery through a topic arrives inside the SNS envelope, whose `Message`
            // carries the document; one straight to the queue IS the document.
            match body["Message"].as_str() {
                Some(nested) => serde_json::from_str(nested).unwrap_or(body),
                None => body,
            }
        })
        .collect()
}

/// Poll the queue — and ONLY the queue — until it has delivered a message carrying
/// `TEST_NOTIFICATION`, or the budget runs out. Nothing in this loop calls Auto Scaling.
async fn await_test_notification(
    router: &MultiServiceRouter,
    case: &str,
    queue: &Queue,
) -> Option<Value> {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let found = drain(router, case, queue)
            .await
            .into_iter()
            .find(|document| document["Event"] == json!(TEST_NOTIFICATION));
        if found.is_some() || Instant::now() >= deadline {
            return found;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the refusal table: what the role's documents say, and whether the put must be
/// refused for it.
struct Row {
    case: &'static str,
    /// Which target the hook names: an index into the targets the test created.
    target: Target,
    /// The role's one inline permissions policy, or `None` for a role carrying none.
    document: Option<String>,
    /// `false` for the one row whose role IAM holds no record of at all.
    held_by_iam: bool,
    refused: bool,
}

/// Which target a row's hook names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// The queue the hook is meant to notify.
    Queue,
    /// Another queue of the same account, which the role may be granted instead.
    OtherQueue,
    /// A topic of the account.
    Topic,
    /// A resource of a vendor this service publishes no notification to.
    Unpublishable,
}

/// RULE: a put that names a notification target is refused — with `ValidationError`, the
/// publish-failure sentence and NO hook stored — exactly when the account's own IAM answers
/// that the hook's role may not publish to that target.
#[tokio::test]
async fn a_hook_is_refused_when_its_role_may_not_publish_to_its_target() {
    let (router, _engine) = emulator();
    let case = "the refusal table";
    let group = "hook-publish-group";
    given_group(&router, case, group).await;
    let queue = given_queue(&router, case, "hook-publish-q").await;
    let other = given_queue(&router, case, "hook-publish-other-q").await;
    let (topic, _subscriber) = given_topic_with_subscriber(&router, case, "hook-publish-t").await;
    // A resource of a vendor this service publishes no notification to: the member admits
    // the value, and there is no publish to authorize for it.
    let unpublishable = format!(
        "arn:aws:lambda:{REGION}:{}:function:hook-publish",
        ::engine::emulator_account_id()
    );

    let rows = vec![
        Row {
            case: "a role allowing the send on that very queue",
            target: Target::Queue,
            document: Some(allow("sqs:SendMessage", &queue.arn)),
            held_by_iam: true,
            refused: false,
        },
        Row {
            case: "a role allowing the send on every resource",
            target: Target::Queue,
            document: Some(allow("sqs:SendMessage", "*")),
            held_by_iam: true,
            refused: false,
        },
        Row {
            case: "a role allowing every action of the queue's service",
            target: Target::Queue,
            document: Some(allow("sqs:*", "*")),
            held_by_iam: true,
            refused: false,
        },
        Row {
            case: "a role carrying no permissions policy at all",
            target: Target::Queue,
            document: None,
            held_by_iam: true,
            refused: true,
        },
        Row {
            case: "a role allowing the send on ANOTHER queue",
            target: Target::Queue,
            document: Some(allow("sqs:SendMessage", &other.arn)),
            held_by_iam: true,
            refused: true,
        },
        Row {
            case: "a role allowing the send beside an explicit deny of it",
            target: Target::Queue,
            document: Some(allow_but_deny("sqs:*", "sqs:SendMessage")),
            held_by_iam: true,
            refused: true,
        },
        Row {
            case: "a role allowing the publish on that topic",
            target: Target::Topic,
            document: Some(allow("sns:Publish", &topic)),
            held_by_iam: true,
            refused: false,
        },
        Row {
            case: "a topic target under a role allowing only the queue action",
            target: Target::Topic,
            document: Some(allow("sqs:SendMessage", "*")),
            held_by_iam: true,
            refused: true,
        },
        Row {
            case: "another queue of the account, under a role allowing the send on it",
            target: Target::OtherQueue,
            document: Some(allow("sqs:SendMessage", &other.arn)),
            held_by_iam: true,
            refused: false,
        },
        Row {
            case: "a target of a vendor this service publishes no notification to",
            target: Target::Unpublishable,
            document: None,
            held_by_iam: true,
            refused: false,
        },
        Row {
            case: "a role this account's IAM holds no record of",
            target: Target::Queue,
            document: None,
            held_by_iam: false,
            refused: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let target = match row.target {
            Target::Queue => queue.arn.clone(),
            Target::OtherQueue => other.arn.clone(),
            Target::Topic => topic.clone(),
            Target::Unpublishable => unpublishable.clone(),
        };
        let role_name = format!("hook-publish-role-{index}");
        let role = if row.held_by_iam {
            given_role(&router, case, &role_name, row.document.as_deref()).await
        } else {
            given_absent_role(&router, case, &role_name).await
        };
        let hook = format!("hook-{index}");
        let (status, code, message) = put_hook(
            &router,
            group,
            &hook,
            &[
                p("LifecycleTransition", TRANSITION),
                p("NotificationTargetARN", target.clone()),
                p("RoleARN", role.clone()),
            ],
        )
        .await;
        let stored = stored_target(&router, case, group, &hook).await;
        if row.refused {
            assert_eq!(
                (status, code.as_str()),
                (400, "ValidationError"),
                "case {case:?}: expected a 400 ValidationError, got {status} {code:?}: \
                 {message:?}"
            );
            assert_eq!(
                message,
                unable_to_publish_message(&target, &role),
                "case {case:?}: expected the publish-failure sentence, got {message:?}"
            );
            assert_eq!(
                stored, None,
                "case {case:?}: a refused put must store no hook, got {stored:?}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} {code:?}: {message:?}"
            );
            assert_eq!(
                stored,
                Some(target.clone()),
                "case {case:?}: the stored hook must carry the target the put named, got \
                 {stored:?}"
            );
        }
    }
}

/// RULE: a target the role may publish to RECEIVES the test message — the
/// `autoscaling:TEST_NOTIFICATION` event for that group — whichever service holds the
/// target.
#[tokio::test]
async fn a_permitted_target_receives_the_test_notification() {
    let (router, _engine) = emulator();
    let case = "a queue named directly";
    let group = "hook-delivery-group";
    given_group(&router, case, group).await;
    let arn = group_arn(&router, case, group).await;

    let queue = given_queue(&router, case, "hook-delivery-q").await;
    let role = given_role(
        &router,
        case,
        "hook-delivery-queue-role",
        Some(&allow("sqs:SendMessage", &queue.arn)),
    )
    .await;
    let (status, code, message) = put_hook(
        &router,
        group,
        "to-the-queue",
        &[
            p("LifecycleTransition", TRANSITION),
            p("NotificationTargetARN", queue.arn.clone()),
            p("RoleARN", role),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} {code:?}: {message:?}"
    );
    let delivered = await_test_notification(&router, case, &queue).await;
    assert_document(case, delivered, group, &arn);

    let case = "a topic a queue is subscribed to";
    let (topic, subscriber) =
        given_topic_with_subscriber(&router, case, "hook-delivery-t").await;
    let role = given_role(
        &router,
        case,
        "hook-delivery-topic-role",
        Some(&allow("sns:Publish", &topic)),
    )
    .await;
    let (status, code, message) = put_hook(
        &router,
        group,
        "to-the-topic",
        &[
            p("LifecycleTransition", TRANSITION),
            p("NotificationTargetARN", topic),
            p("RoleARN", role),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} {code:?}: {message:?}"
    );
    let delivered = await_test_notification(&router, case, &subscriber).await;
    assert_document(case, delivered, group, &arn);
}

/// Every member a test message must carry, asserted on the document the target received.
fn assert_document(case: &str, delivered: Option<Value>, group: &str, arn: &str) {
    let document = delivered.unwrap_or_else(|| {
        panic!("case {case:?}: the target must receive one {TEST_NOTIFICATION} message")
    });
    let expected: &[(&str, &str)] = &[
        ("Event", TEST_NOTIFICATION),
        ("AutoScalingGroupName", group),
        ("AutoScalingGroupARN", arn),
        ("Service", NOTIFICATION_SERVICE),
        ("AccountId", ::engine::emulator_account_id()),
    ];
    for (member, value) in expected {
        let actual = &document[member];
        assert_eq!(
            actual,
            &json!(value),
            "case {case:?}: expected {member} == {value:?}, got {actual:?} in {document}"
        );
    }
    for member in ["RequestId", "Time"] {
        let actual = document[member].as_str().unwrap_or_default();
        assert!(
            !actual.is_empty(),
            "case {case:?}: expected a {member} in {document}, got {actual:?}"
        );
    }
}

/// RULE: the pair the publish is attempted with is the one the hook WOULD STAND with, so an
/// update that names only a target is proved under the role the stored hook carries and one
/// that names only a role is proved against the target it carries — and a refused update
/// leaves every member of that hook as it was.
#[tokio::test]
async fn an_update_naming_one_half_of_the_pair_is_proved_against_the_other() {
    let (router, _engine) = emulator();
    let case = "the hook is created with a role granted on one queue only";
    let group = "hook-inherited-role-group";
    given_group(&router, case, group).await;
    let granted = given_queue(&router, case, "hook-inherited-granted-q").await;
    let withheld = given_queue(&router, case, "hook-inherited-withheld-q").await;
    let role = given_role(
        &router,
        case,
        "hook-inherited-role",
        Some(&allow("sqs:SendMessage", &granted.arn)),
    )
    .await;
    let hook = "inherited";
    let (status, code, message) = put_hook(
        &router,
        group,
        hook,
        &[
            p("LifecycleTransition", TRANSITION),
            p("NotificationTargetARN", granted.arn.clone()),
            p("RoleARN", role.clone()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} {code:?}: {message:?}"
    );

    let case = "an update naming a target the stored role may not publish to";
    let (status, code, message) = put_hook(
        &router,
        group,
        hook,
        &[p("NotificationTargetARN", withheld.arn.clone())],
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "ValidationError"),
        "case {case:?}: expected a 400 ValidationError, got {status} {code:?}: {message:?}"
    );
    assert_eq!(
        message,
        unable_to_publish_message(&withheld.arn, &role),
        "case {case:?}: the refusal must name the target the update carried and the role the \
         stored hook carries, got {message:?}"
    );
    let stored = stored_target(&router, case, group, hook).await;
    assert_eq!(
        stored,
        Some(granted.arn.clone()),
        "case {case:?}: a refused update must leave the stored target alone, got {stored:?}"
    );

    let case = "an update naming the target that role IS granted on";
    let (status, code, message) = put_hook(
        &router,
        group,
        hook,
        &[p("NotificationTargetARN", granted.arn.clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} {code:?}: {message:?}"
    );
    let delivered = await_test_notification(&router, case, &granted).await;
    assert!(
        delivered.is_some(),
        "case {case:?}: the target must receive a {TEST_NOTIFICATION} for the update too"
    );

    // The other half of the pair, named on its own: an update that points the hook's standing
    // target at ANOTHER role is proved against that role, and one that points it at a role
    // granted on the target is served and delivers to it.
    let case = "an update naming only a role that may not publish to the standing target";
    let withholding = given_role(&router, case, "hook-inherited-other-role", None).await;
    let (status, code, message) =
        put_hook(&router, group, hook, &[p("RoleARN", withholding.clone())]).await;
    assert_eq!(
        (status, code.as_str()),
        (400, "ValidationError"),
        "case {case:?}: expected a 400 ValidationError, got {status} {code:?}: {message:?}"
    );
    assert_eq!(
        message,
        unable_to_publish_message(&granted.arn, &withholding),
        "case {case:?}: the refusal must name the target the stored hook carries and the role \
         the update carried, got {message:?}"
    );

    let case = "an update naming only a role that may publish to the standing target";
    let permitted = given_role(
        &router,
        case,
        "hook-inherited-second-role",
        Some(&allow("sqs:SendMessage", &granted.arn)),
    )
    .await;
    let (status, code, message) =
        put_hook(&router, group, hook, &[p("RoleARN", permitted)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} {code:?}: {message:?}"
    );
    let delivered = await_test_notification(&router, case, &granted).await;
    assert!(
        delivered.is_some(),
        "case {case:?}: the standing target must receive a {TEST_NOTIFICATION} for the role \
         the update pointed it at"
    );
}
