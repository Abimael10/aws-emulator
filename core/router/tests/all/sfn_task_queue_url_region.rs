// SPDX-License-Identifier: CC0-1.0

//! WHERE a Step Functions `Task` state's Amazon SQS integration call is ADDRESSED when the
//! `QueueUrl` it names its target by carries no Region of its own — driven across Step
//! Functions, IAM and Amazon SQS, mounted on ONE engine exactly as `core/router/src/lib.rs`
//! mounts them, and borrowing the wiring `sfn_task_target_authorization` already owns.
//!
//! RULE: a request that addresses its target by a url names the queue that url's
//! `/<accountId>/<QueueName>` path carries, in the Region its authority names, and the
//! access request the call is subject to is over THAT queue's ARN; a url whose authority
//! names no Region — which is every url this emulator mints for a client, since one endpoint
//! here answers for every Region — addresses the queue of that name this emulator holds,
//! preferring the caller's own Region where it holds one.
//!
//! So a `Task` state handed the url its client was given reaches the queue that url names
//! whichever Region created it, the grant the role needs is the one on the ARN
//! `GetQueueAttributes` reports for that queue, and a role holding no such grant is refused
//! over that same ARN with nothing delivered.
//!
//! The tables span what the rule generalises over: the REGION the queue was created in (the
//! execution's own and four others, from four continents), the integration FAMILY the `Task`
//! names the send through and the member SPELLING each family writes, whether the caller's
//! own Region holds a queue of the name as well (the one case where one url string names two
//! queues), and — for the authorization half — which ARN the role's one grant names.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sfn_task_target_authorization::{
    allow, emulator, logs_only, machine, masked, ran, refusal_metadata, role, session_principal,
    string, task_failure, ACCOUNT, FORBIDDEN, REGION, SQS_REFUSAL_CODE,
};

/// The `host[:port]` every Amazon SQS call below arrives on: an ADDRESS of this process
/// rather than one of AWS's own endpoint names, which is what a client pointed at this
/// emulator sends — and what makes the url a create answers carry no Region label at all.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// The body every row's message carries, so a row reads back what it sent.
const BODY: &str = "addressed-by-url";

/// The name Amazon SQS's own SDK client carries, which is what the cause of a refused
/// optimized integration reports the service under.
const SQS_CLIENT: &str = "AmazonSQS";

/// How long a row waits for a message to arrive on a queue. Generous, so a loaded host
/// cannot fail a row the emulator is still working through.
const PATIENCE: Duration = Duration::from_secs(20);

/// How long one look at a queue waits before looking again.
const POLL: Duration = Duration::from_millis(25);

/// One Amazon SQS call made IN `region` — the credential scope's Region, which is what the
/// service reads a request's Region from — ARRIVING on this emulator's own endpoint, which
/// is where a client configured with it sends every call whatever Region it is in.
async fn sqs_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", EMULATOR_HOST)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{region}/sqs/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {operation} in {region}: expected 200, got {status} with {parsed:?}"
    );
    parsed
}

/// The queue called `name` in `region`, as that Region's own create answers it: its url,
/// and the ARN `GetQueueAttributes` reports for it — the resource a grant of
/// `sqs:SendMessage` on that queue names.
async fn queue_in(router: &MultiServiceRouter, region: &str, name: &str) -> (String, String) {
    let created = sqs_in(router, region, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = string(&created, "QueueUrl");
    let attributes = sqs_in(
        router,
        region,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("GetQueueAttributes answers the QueueArn, got {attributes}"))
        .to_string();
    (url, arn)
}

/// The ARN of the queue called `name` in `region`, whether or not any queue of that name is
/// held there: what a statement written for it names
/// (`arn:aws:sqs:<region>:<account>:<QueueName>`).
fn queue_arn(region: &str, name: &str) -> String {
    format!("arn:aws:sqs:{region}:{ACCOUNT}:{name}")
}

/// The bodies the queue at `url` holds, read in `region`: looked at again until one arrives
/// or [`PATIENCE`] runs out, so a row waits for the CONDITION rather than for a span. What
/// an empty answer means is the caller's own assertion.
async fn bodies_in(router: &MultiServiceRouter, region: &str, url: &str) -> Vec<String> {
    let began = Instant::now();
    loop {
        let received = sqs_in(
            router,
            region,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let bodies: Vec<String> = received
            .get("Messages")
            .and_then(Value::as_array)
            .map(|messages| {
                messages
                    .iter()
                    .map(|message| string(message, "Body"))
                    .collect()
            })
            .unwrap_or_default();
        if !bodies.is_empty() || began.elapsed() >= PATIENCE {
            return bodies;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// How many messages the queue at `url` holds in `region`, as its own attribute reports:
/// the reading for a queue that must stay EMPTY, which no receive can prove.
async fn depth_in(router: &MultiServiceRouter, region: &str, url: &str) -> String {
    let reported = sqs_in(
        router,
        region,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["ApproximateNumberOfMessages"] }),
    )
    .await;
    reported["Attributes"]["ApproximateNumberOfMessages"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Which integration family a row names the send through, which also decides the SPELLING
/// of the members its `Parameters` carry: the optimized integration documents them in its
/// own PascalCase, and the AWS SDK family takes the invoked operation's own members.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Optimized,
    AwsSdk,
}

impl Family {
    /// The `Resource` a `Task` of this family names.
    fn resource(self) -> &'static str {
        match self {
            Self::Optimized => "arn:aws:states:::sqs:sendMessage",
            Self::AwsSdk => "arn:aws:states:::aws-sdk:sqs:sendMessage",
        }
    }

    /// The parameters it carries for a send of [`BODY`] to `url`.
    fn parameters(self, url: &str) -> Value {
        match self {
            Self::Optimized => json!({ "QueueUrl": url, "MessageBody": BODY }),
            Self::AwsSdk => json!({ "queueUrl": url, "messageBody": BODY }),
        }
    }
}

/// A url this emulator minted for a client carries no Region: it is an address of this
/// process, which answers for every Region, so nothing of the Region can be read off it.
fn names_no_region(url: &str, case: &str) {
    assert!(
        url.starts_with(&format!("https://{EMULATOR_HOST}/")),
        "case {case:?}: expected a url on the endpoint the create reached, got {url:?}"
    );
    assert!(
        !url.contains("amazonaws.com"),
        "case {case:?}: expected a url naming no regional endpoint, got {url:?}"
    );
}

/// A role whose one grant beyond its log writes is `sqs:SendMessage` on `resource`.
async fn sending_role(
    router: &MultiServiceRouter,
    name: &str,
    resource: &str,
) -> String {
    role(
        router,
        name,
        &json!({
            "Version": "2012-10-17",
            "Statement": [logs_only(), allow("sqs:SendMessage", resource)],
        }),
    )
    .await
}

/// RULE: the send reaches the queue the url names, in whichever Region that queue was
/// created in, and the grant it needs is the one on that queue's own ARN.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_by_a_region_less_url_reaches_the_queue_that_url_names() {
    // (case, the Region the queue lives in, the family the Task names the send through)
    let rows: [(&str, &str, Family); 5] = [
        (
            "the execution's own Region, whose url names it no more than any other",
            REGION,
            Family::Optimized,
        ),
        (
            "another Region of the same continent",
            "us-east-1",
            Family::Optimized,
        ),
        (
            "a Region on another continent, through the AWS SDK family",
            "eu-west-1",
            Family::AwsSdk,
        ),
        (
            "a Region on a third continent",
            "ap-northeast-1",
            Family::Optimized,
        ),
        (
            "a fourth, through the AWS SDK family",
            "sa-east-1",
            Family::AwsSdk,
        ),
    ];

    let router = emulator();

    for (index, (case, queue_region, family)) in rows.into_iter().enumerate() {
        let name = format!("by-url-{index}");
        let (url, arn) = queue_in(&router, queue_region, &name).await;
        names_no_region(&url, case);
        assert_eq!(
            arn,
            queue_arn(queue_region, &name),
            "case {case:?}: expected the ARN of the Region the create addressed"
        );
        let role_arn = sending_role(&router, &format!("ByUrlRole{index}"), &arn).await;
        let machine_arn = machine(
            &router,
            &format!("by-url-{index}"),
            family.resource(),
            &family.parameters(&url),
            &role_arn,
        )
        .await;
        let (_execution_arn, described) = ran(&router, &machine_arn, case).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("SUCCEEDED"), None),
            "case {case:?}: expected SUCCEEDED, got {described:?}"
        );
        let bodies = bodies_in(&router, queue_region, &url).await;
        assert_eq!(
            bodies,
            vec![BODY.to_string()],
            "case {case:?}: expected the message on the queue of {queue_region}, got {bodies:?}"
        );
    }
}

/// RULE, at its boundary: one url string names two queues when the caller's own Region
/// holds the same name, and the queue that Region holds is the one its endpoint serves —
/// so that is where the send goes, and the queue of the other Region is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_region_less_url_is_served_by_the_callers_own_region_when_it_holds_the_name() {
    let router = emulator();
    let case = "the same name in the execution's Region and in another";
    let name = "held-in-both";
    let (near_url, near_arn) = queue_in(&router, REGION, name).await;
    let (far_url, far_arn) = queue_in(&router, "eu-central-1", name).await;
    names_no_region(&near_url, case);
    // The point of the row: both queues are addressed by one identical string, so nothing
    // of the url tells them apart.
    assert_eq!(
        near_url, far_url,
        "case {case:?}: expected one url to address both queues of the name"
    );

    let role_arn = role(
        &router,
        "HeldInBothRole",
        &json!({
            "Version": "2012-10-17",
            "Statement": [
                logs_only(),
                allow("sqs:SendMessage", &near_arn),
                allow("sqs:SendMessage", &far_arn),
            ],
        }),
    )
    .await;
    let machine_arn = machine(
        &router,
        "held-in-both",
        Family::Optimized.resource(),
        &Family::Optimized.parameters(&near_url),
        &role_arn,
    )
    .await;
    let (_execution_arn, described) = ran(&router, &machine_arn, case).await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected SUCCEEDED, got {described:?}"
    );
    let bodies = bodies_in(&router, REGION, &near_url).await;
    assert_eq!(
        bodies,
        vec![BODY.to_string()],
        "case {case:?}: expected the message on the queue of {REGION}, got {bodies:?}"
    );
    let depth = depth_in(&router, "eu-central-1", &far_url).await;
    assert_eq!(
        depth, "0",
        "case {case:?}: expected the queue of the other Region to stay empty, got {depth} \
         message(s)"
    );
}

/// RULE, its authorization half: the access request a region-less send is subject to is
/// over the ARN of the queue the url really names, so a grant on that ARN allows the call
/// and a grant on the same name in the execution's own Region — another resource — does
/// not; and the refusal is the one Amazon SQS itself would have answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_region_less_sends_access_request_is_over_the_far_queues_own_arn() {
    /// What a row grants `sqs:SendMessage` on.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Grant {
        /// The ARN of the queue the url names, in the Region that holds it.
        FarQueue,
        /// The same NAME in the Region the execution runs in, which is another resource —
        /// and one no queue of this account answers to at all.
        NearName,
        /// Nothing about the action.
        Unrelated,
    }

    // (case, the Region the queue lives in, what the role grants, whether the send is made)
    let rows: [(&str, &str, Grant, bool); 4] = [
        (
            "the ARN of the queue the url names",
            "us-east-1",
            Grant::FarQueue,
            true,
        ),
        (
            "the same name in the execution's Region, which is another resource",
            "us-east-1",
            Grant::NearName,
            false,
        ),
        ("nothing about the action", "eu-west-1", Grant::Unrelated, false),
        (
            "the ARN of a queue on a third continent",
            "ap-southeast-2",
            Grant::FarQueue,
            true,
        ),
    ];

    let router = emulator();

    for (index, (case, queue_region, grant, allowed)) in rows.into_iter().enumerate() {
        let name = format!("authorized-by-url-{index}");
        let (url, far_arn) = queue_in(&router, queue_region, &name).await;
        names_no_region(&url, case);
        let statements = match grant {
            Grant::FarQueue => vec![logs_only(), allow("sqs:SendMessage", &far_arn)],
            Grant::NearName => vec![
                logs_only(),
                allow("sqs:SendMessage", &queue_arn(REGION, &name)),
            ],
            Grant::Unrelated => vec![logs_only()],
        };
        let role_arn = role(
            &router,
            &format!("AuthorizedByUrlRole{index}"),
            &json!({ "Version": "2012-10-17", "Statement": statements }),
        )
        .await;
        let machine_arn = machine(
            &router,
            &format!("authorized-by-url-{index}"),
            Family::Optimized.resource(),
            &Family::Optimized.parameters(&url),
            &role_arn,
        )
        .await;
        let (execution_arn, described) = ran(&router, &machine_arn, case).await;

        if allowed {
            assert_eq!(
                described.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: expected SUCCEEDED, got {described:?}"
            );
            let bodies = bodies_in(&router, queue_region, &url).await;
            assert_eq!(
                bodies,
                vec![BODY.to_string()],
                "case {case:?}: expected the message on the queue of {queue_region}, got \
                 {bodies:?}"
            );
            continue;
        }

        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some("SQS.AmazonSQSException")),
            "case {case:?}: expected the send to be refused, got {described:?}"
        );
        // The refusal is Amazon SQS's own answer to an unauthorized call: it names the
        // session the call was made as, the action in the lower case that service's
        // authorization layer reports, and the ARN of the queue the url really names —
        // never one in the Region the execution happens to run in.
        let details = task_failure(&router, &execution_arn, case).await;
        let cause = string(&details, "cause");
        let principal = session_principal(&role_arn, &cause);
        let opening = format!(
            "User: {principal} is not authorized to perform: sqs:sendmessage on resource: \
             {far_arn} because no identity-based policy allows the sqs:sendmessage action"
        );
        assert!(
            cause.starts_with(&opening),
            "case {case:?}: expected the cause to open {opening:?}, got {cause:?}"
        );
        let closing = refusal_metadata(
            Family::Optimized.resource(),
            SQS_CLIENT,
            SQS_REFUSAL_CODE,
            FORBIDDEN,
        );
        let reported = masked(&cause);
        assert!(
            reported.ends_with(&closing),
            "case {case:?}: expected the cause to close {closing:?}, got {reported:?}"
        );
        // And nothing was sent: the call the policies withheld was never made.
        let depth = depth_in(&router, queue_region, &url).await;
        assert_eq!(
            depth, "0",
            "case {case:?}: expected the queue to stay empty, got {depth} message(s)"
        );
    }
}
