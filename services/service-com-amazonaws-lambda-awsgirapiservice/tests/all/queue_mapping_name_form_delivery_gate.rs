// SPDX-License-Identifier: CC0-1.0

//! `Enabled` as the delivery gate of an Amazon SQS event source mapping, driven HOWEVER the
//! mapping named the function it invokes.
//!
//! The RULE pinned here: **a mapping is made on "the name or ARN of the Lambda function"
//! (`smithy_query(query = "doc", shape = "CreateEventSourceMappingRequest", member =
//! "FunctionName")`: a bare function name, that function's ARN, or the partial ARN
//! `<accountId>:function:<name>`), and every one of those forms names the SAME function — so
//! the mapping made under any of them polls its queue and invokes that function while it
//! reports `Enabled` ("when true, the event source mapping is active. When false, Lambda
//! pauses polling and invocation"), takes nothing out of the queue while it reports
//! `Disabled` — the message sent meanwhile stays WAITING there and a client's own
//! `ReceiveMessage` is still handed it — and hands exactly that withheld message over once it
//! is enabled again.** How the request spelled the function is a matter for the CREATE, not
//! for the data plane: a mapping addressed by ARN is the form a deploying client writes, and
//! it has to poll exactly as a mapping addressed by name does.
//!
//! The rows differ along what the rule generalises over: the name form the
//! `CreateEventSourceMapping` request carried (name, function ARN, partial ARN).
//!
//! The wiring under every row is the one a client deploys rather than a shortcut: the
//! execution role is written through IAM's own `CreateRole`/`AttachRolePolicy`/`PutRolePolicy`
//! and carries the policies AWS PUBLISHES for a queue-triggered function
//! (`AWSLambdaBasicExecutionRole`, `AWSLambdaSQSQueueExecutionRole`) beside the one inline
//! grant the handler's own send needs, the function is a real `python3.12` .zip whose handler
//! forwards each record it is handed to a second queue with the AWS SDK, and the source queue
//! hides a delivered message for longer than that function may run. So a delivery observed
//! here is one the poller made as that role, handed to a handler that ran, and reported by a
//! client reading the queue the handler wrote.
//!
//! Every wait is on a CONDITION with a generous deadline, and the absence leg keeps LOOKING
//! over a window of many poll intervals rather than trusting one look, so a loaded host
//! delays a row instead of weakening it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::sqs_poller::POLL_INTERVAL;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{given_role, iam};
use crate::event_source_mappings::{
    await_function_active, await_mapping_state, call, call_sqs, create_mapping, create_queue,
    get_mapping, queue_message_counts, router_on, send_message, taken_bodies, LOOK_AGAIN,
    MAPPINGS_URI, RUNNING_HANDLER, RUNNING_RUNTIME, STATE_DISABLED, STATE_ENABLED,
};

/// Where `CreateFunction` is bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The policies AWS publishes for a queue-triggered function's execution role: the log writes
/// every execution role carries, and the three queue calls Lambda makes to consume a queue on
/// the function's behalf. Attached here by ARN, so the documents in force are the ones IAM
/// resolves for them.
const PUBLISHED_POLICIES: [&str; 2] = [
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole",
    "arn:aws:iam::aws:policy/service-role/AWSLambdaSQSQueueExecutionRole",
];

/// The source queue's `VisibilityTimeout` in seconds, and the `Timeout` of the function it
/// maps onto: a queue maps onto a function only while it outlasts that function's timeout.
const VISIBILITY_TIMEOUT: i64 = 30;
/// See [`VISIBILITY_TIMEOUT`].
const FUNCTION_TIMEOUT: i64 = 10;

/// The environment variable the handler reads the queue it forwards to from.
const RESULTS_QUEUE_URL: &str = "RESULTS_QUEUE_URL";

/// The body delivered while the mapping polls, and the one sent while it is paused.
const WARM_UP: &str = "delivered-while-enabled";
/// See [`WARM_UP`].
const WITHHELD: &str = "sent-while-paused";

/// How long an observation of work the emulator owes — a batch polled, a handler run, a
/// message forwarded, a state settled — waits before it is a failure. Generous, and taken
/// from what the emulator allows ONE attempt ([`PERMIT_WAIT`], [`HOST_WAIT_CEILING`]) rather
/// than picked here, because a whole-workspace run shares its cores with every other suite.
const BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(Duration::from_secs(60));

/// How many of the poller's own intervals the absence leg keeps looking for a delivery the
/// rule forbids: well past the one pass that would have made it.
const QUIET_PASSES: u32 = 12;

/// The handler every row runs: for the batch it is handed it forwards ONE document naming
/// every record's body and how many there were, so a delivery that happened leaves a document
/// a client reads off the results queue and one that did not leaves nothing. The send is the
/// handler's OWN AWS SDK call, made as the function's execution role.
const FORWARDING_HANDLER: &str = r#"
import json
import os

import boto3


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    bodies = [record["body"] for record in records]
    boto3.client("sqs").send_message(
        QueueUrl=os.environ["RESULTS_QUEUE_URL"],
        MessageBody=json.dumps({"bodies": bodies, "count": len(bodies)}),
    )
    return {"batchItemFailures": []}
"#;

/// Lambda over the SAME engine as Amazon SQS and IAM: the mapping resolves its role in the
/// service that owns roles and its queue in the service that owns queues, exactly what
/// `core/router` assembles.
fn router_with_queues_and_iam() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// How one row's `CreateEventSourceMapping` names the function it maps.
#[derive(Clone, Copy, Debug)]
enum NameForm {
    /// "Function name – `MyFunction`".
    Name,
    /// "Function ARN – `arn:aws:lambda:us-west-2:123456789012:function:MyFunction`".
    FunctionArn,
    /// "Partial ARN – `123456789012:function:MyFunction`".
    PartialArn,
}

impl NameForm {
    /// What the request carries as its `FunctionName`, given the function's own name and the
    /// ARN `CreateFunction` reported for it.
    fn requested(self, name: &str, arn: &str) -> String {
        match self {
            Self::Name => name.to_string(),
            Self::FunctionArn => arn.to_string(),
            // The ARN without its `arn:<partition>:lambda:<region>:` prefix, which is what
            // the documented partial form is.
            Self::PartialArn => arn
                .split(':')
                .skip(4)
                .collect::<Vec<&str>>()
                .join(":"),
        }
    }

    /// A short label for this row's names and messages.
    fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::FunctionArn => "arn",
            Self::PartialArn => "partial-arn",
        }
    }
}

/// Every name form the table drives.
const NAME_FORMS: [NameForm; 3] = [
    NameForm::Name,
    NameForm::FunctionArn,
    NameForm::PartialArn,
];

/// The execution role a row's function runs as: created through IAM with the published
/// policies attached and one inline grant for the send the handler makes. Answers its ARN.
async fn given_consumer_role(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    results_arn: &str,
) -> String {
    let role_arn = given_role(
        router,
        case,
        role,
        Some(json!([{
            "Effect": "Allow",
            "Action": ["sqs:SendMessage"],
            "Resource": results_arn,
        }])),
    )
    .await;
    for policy in PUBLISHED_POLICIES {
        let (status, attached) = iam(
            router,
            "AttachRolePolicy",
            &[("RoleName", role), ("PolicyArn", policy)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: AWS publishes {policy}, so attaching it must be served, got \
             {status} {attached}"
        );
    }
    role_arn
}

/// A function whose package RUNS [`FORWARDING_HANDLER`] as `role_arn`, waited out to
/// `Active`; answers the ARN `CreateFunction` reported for it.
async fn given_forwarding_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    results_url: &str,
) -> String {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNNING_RUNTIME,
            "Handler": RUNNING_HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": { "Variables": { RESULTS_QUEUE_URL: results_url } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(crate::synchronous_invoke_run::package(FORWARDING_HANDLER)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    let arn = created.string("FunctionArn").unwrap_or_else(|| {
        panic!(
            "case {case:?}: CreateFunction reported no FunctionArn: {}",
            created.text()
        )
    });
    await_function_active(router, case, name).await;
    arn
}

/// Every document the handler has forwarded to the queue `url` addresses so far, taken off it
/// as they are read so a later look finds only what arrived since.
async fn forwarded_documents(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    taken_bodies(router, case, url)
        .await
        .into_iter()
        .map(|body| {
            serde_json::from_str(&body).unwrap_or_else(|error| {
                panic!("case {case:?}: the handler forwarded {body:?}, which is not JSON: {error}")
            })
        })
        .collect()
}

/// Wait until the handler has forwarded a document whose `bodies` hold `body`, answering
/// every document seen on the way.
async fn awaited_forward(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    body: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    let mut seen: Vec<Value> = Vec::new();
    loop {
        seen.extend(forwarded_documents(router, case, url).await);
        let delivered = seen
            .iter()
            .any(|document| document["bodies"] == json!([body]));
        if delivered {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the handler to be handed {body:?} and forward it, it \
             forwarded {seen:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `UpdateEventSourceMapping` setting `Enabled`, waited out to the state that setting asks
/// for.
async fn set_enabled(router: &MultiServiceRouter, case: &str, uuid: &str, enabled: bool) {
    let answered = call(
        router,
        "PUT",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &json!({ "Enabled": enabled }),
    )
    .await;
    assert_eq!(
        answered.status,
        202,
        "case {case:?}: expected UpdateEventSourceMapping(Enabled={enabled}) to answer 202, got \
         {} {}",
        answered.status,
        answered.text()
    );
    let wanted = if enabled {
        STATE_ENABLED
    } else {
        STATE_DISABLED
    };
    await_mapping_state(router, case, uuid, wanted).await;
}

/// Every body a client's own `ReceiveMessage` is handed off the queue `url` addresses,
/// WITHOUT taking any of them out of it: the batch is handed back at once
/// (`VisibilityTimeout` 0), so this read leaves the queue as it found it.
async fn peeked_bodies(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let (status, answered) = call_sqs(
        router,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "VisibilityTimeout": 0,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ReceiveMessage on {url} to answer 200, got {status} {answered}"
    );
    answered["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// RULE: however its `FunctionName` named the function, a mapping polls its queue and invokes
/// that function while it reports `Enabled`, withholds the queue while it reports `Disabled`,
/// and hands the withheld message over once it is enabled again.
#[tokio::test]
async fn a_queue_mapping_polls_and_pauses_however_its_function_was_named() {
    for form in NAME_FORMS {
        let case = format!("a mapping made on the function's {}", form.label());
        let case = case.as_str();
        let label = form.label();
        let router = router_with_queues_and_iam();
        let (results_url, results_arn) =
            create_queue(&router, case, &format!("named-results-{label}"), &json!({})).await;
        let (source_url, source_arn) = create_queue(
            &router,
            case,
            &format!("named-source-{label}"),
            &json!({ "VisibilityTimeout": VISIBILITY_TIMEOUT.to_string() }),
        )
        .await;
        let role_arn = given_consumer_role(
            &router,
            case,
            &format!("named-consumer-{label}"),
            &results_arn,
        )
        .await;
        let function = format!("named-consumer-fn-{label}");
        let function_arn =
            given_forwarding_function(&router, case, &function, &role_arn, &results_url).await;

        let created = create_mapping(
            &router,
            &json!({
                "FunctionName": form.requested(&function, &function_arn),
                "EventSourceArn": source_arn,
                "BatchSize": 1,
                "Enabled": true,
            }),
        )
        .await;
        assert_eq!(
            created.status,
            202,
            "case {case:?}: expected CreateEventSourceMapping to answer 202, got {} {}",
            created.status,
            created.text()
        );
        let uuid = created
            .string("UUID")
            .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", created.text()));
        await_mapping_state(&router, case, &uuid, STATE_ENABLED).await;

        // ROUND TRIP: whichever form named it, the mapping a client reads back is on THIS
        // function — which is what makes the delivery below that function's own.
        let read = get_mapping(&router, &uuid).await;
        assert_eq!(
            read.string("FunctionArn").as_deref(),
            Some(function_arn.as_str()),
            "case {case:?}: the mapping must report the function it names, got {}",
            read.text()
        );

        // Polling: the message sent now reaches the handler, which forwards it.
        send_message(&router, case, &source_url, WARM_UP, None).await;
        let forwarded = awaited_forward(&router, case, &results_url, WARM_UP).await;
        assert_eq!(
            forwarded.len(),
            1,
            "case {case:?}: one message is one batch of at most BatchSize records, so the \
             handler must have forwarded exactly one document, it forwarded {forwarded:?}"
        );
        assert_eq!(
            forwarded[0],
            json!({ "bodies": [WARM_UP], "count": 1 }),
            "case {case:?}: the handler must report the batch it was handed, it forwarded {}",
            forwarded[0]
        );

        // Paused: the message sent now stays WAITING on the source queue — no consumer took
        // it — a client's own read is still handed it, and the handler is handed nothing.
        set_enabled(&router, case, &uuid, false).await;
        send_message(&router, case, &source_url, WITHHELD, None).await;
        let quiet_until = Instant::now() + POLL_INTERVAL * QUIET_PASSES;
        while Instant::now() < quiet_until {
            let forwarded = forwarded_documents(&router, case, &results_url).await;
            assert!(
                forwarded.is_empty(),
                "case {case:?}: a paused mapping invokes nothing, so the handler can forward \
                 nothing, but it forwarded {forwarded:?}"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        let counts = queue_message_counts(&router, case, &source_url).await;
        assert_eq!(
            counts,
            (1, 0, 0),
            "case {case:?}: a paused mapping polls nothing, so the message must be waiting on \
             the source queue, got {counts:?} as (visible, in flight, delayed)"
        );
        let waiting = peeked_bodies(&router, case, &source_url).await;
        assert_eq!(
            waiting,
            vec![WITHHELD.to_string()],
            "case {case:?}: the withheld message must still be handed to a client's own \
             ReceiveMessage, it was handed {waiting:?}"
        );

        // Enabled again: the same mapping hands that message over, and the queue drains.
        set_enabled(&router, case, &uuid, true).await;
        let delivered = awaited_forward(&router, case, &results_url, WITHHELD).await;
        assert_eq!(
            delivered,
            vec![json!({ "bodies": [WITHHELD], "count": 1 })],
            "case {case:?}: the mapping enabled again must hand the handler the withheld \
             message and nothing else, it forwarded {delivered:?}"
        );
        let drained = {
            let deadline = Instant::now() + BUDGET;
            let mut counts = queue_message_counts(&router, case, &source_url).await;
            while counts != (0, 0, 0) && Instant::now() < deadline {
                tokio::time::sleep(LOOK_AGAIN).await;
                counts = queue_message_counts(&router, case, &source_url).await;
            }
            counts
        };
        assert_eq!(
            drained,
            (0, 0, 0),
            "case {case:?}: a record the handler processed is deleted from the queue, so the \
             source queue must drain, got {drained:?} as (visible, in flight, delayed)"
        );
    }
}
