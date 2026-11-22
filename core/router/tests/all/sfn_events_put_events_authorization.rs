// SPDX-License-Identifier: CC0-1.0

//! The EXECUTION ROLE an event-publication `Task` state is performed as, over the bus each
//! of the publication's entries names — driven across the four services the question spans,
//! mounted on ONE engine.
//!
//! RULE: Step Functions makes a `Task` state's integration call ACTING AS the state
//! machine's `roleArn`, so the identity-based policies in force on that role decide whether
//! the call is made at all, over the `(action, resource)` pair the call itself forms: for a
//! publication that is `events:PutEvents` on the event bus the entry names — "the name or
//! ARN of the event bus to receive the event [...] If you omit this, the default event bus
//! is used" — one access request per bus the request's entries name, so a role granted one
//! bus may not publish onto another and a batch naming a bus it was not granted is not
//! published at all. A call the policies do not allow is NOT made: the task fails with the
//! invoked service's own access-denied refusal, reported under that service's BASE SDK
//! exception (`PutEvents` models no access-denied error of its own), the bus receives
//! nothing, and no rule of it is matched. A call they DO allow is made, and the entry
//! reaches the bus's rules.
//!
//! Four crates have to agree for that to hold — Step Functions decides and makes the call,
//! IAM holds the documents it is judged against, EventBridge owns the buses and routes what
//! was published, and Amazon SQS is where the rule's target delivery is read — so the suite
//! lives here rather than in any of them.
//!
//! The rows differ along what the rule generalises over: which bus the entry names (the
//! default one it names by omission, a custom one by name, the same one by ARN), which
//! integration family names the publication (the optimized one and the AWS SDK one, which
//! make the same call on the same bus), how many entries the batch carries and whether the
//! role's grant covers every one of their buses, and whether the grant names the bus or
//! every resource.
//!
//! The harness it drives all this through — the role, statement and member readers, the
//! state machine and the execution poller — is `all/sfn_task_target_authorization.rs`, which
//! owns the same rule over the integrations whose target is a queue or a function:
//! `tests/all/` is one crate root, so this suite borrows them rather than carrying a second
//! copy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sfn_task_target_authorization::{
    allow, logs_only, machine, masked, ran, refusal_metadata, role, session_principal, sqs, string,
    task_failure, ACCOUNT, REFUSAL_CODE, REGION,
};

/// The status a publication the role may not make leaves its execution in, and the one a
/// publication it may make leaves it in (`ExecutionStatus`).
const FAILED: &str = "FAILED";
const SUCCEEDED: &str = "SUCCEEDED";

/// The error name a `Task` reports for a refused publication: Amazon EventBridge as Step
/// Functions names it in a task error, and the base exception class its client resolves an
/// answer no error shape of that service's model names into — `PutEvents` declares only
/// `InternalException` (`smithy_query(query="operation",operation="PutEvents",model_path=
/// "services/service-com-amazonaws-eventbridge-awsevents/smithy-model/
/// com-amazonaws-eventbridge-awsevents.json")`), so an authorization refusal is one such
/// answer.
const REFUSAL_ERROR: &str = "EventBridge.AmazonEventBridgeException";

/// The names the client each integration family is served by gives the service in the
/// metadata of a relayed exception: the earlier generation of client an optimized `Resource`
/// is served through carries its own name, and the current AWS SDK names the service as that
/// SDK does.
const OPTIMIZED_SERVICE: &str = "AmazonEventBridge";
/// See [`OPTIMIZED_SERVICE`].
const SDK_SERVICE: &str = "EventBridge";

/// The status Amazon EventBridge's front door answers an unauthorized call with.
const REFUSAL_STATUS: u16 = 400;

/// The IAM action a publication is taken under, as a refusal spells it.
const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// The clause a refusal of a role carrying no grant at all ends with.
const NO_IDENTITY_GRANT: &str = "because no identity-based policy allows the events:PutEvents \
                                 action";

/// The custom bus a row may name, beside the `default` bus every account carries.
const CUSTOM_BUS: &str = "orders-bus";
/// See [`CUSTOM_BUS`].
const DEFAULT_BUS: &str = "default";

/// The `source` and `detail-type` every entry below publishes under, which is what the rules
/// of both buses match on.
const SOURCE: &str = "sim.sfn.publication";
const DETAIL_TYPE: &str = "publication.attempted";

/// The member each entry's `Detail` carries its row's own marker under, so a delivered
/// envelope is attributed to the publication that made it.
const ROW_MEMBER: &str = "row";

/// How long one poll waits before looking again, and how long this suite waits for work the
/// clock and the four services owe: generous, so a loaded host cannot decide the outcome.
const POLL: Duration = Duration::from_millis(25);
/// See [`POLL`].
const PATIENCE: Duration = Duration::from_secs(120);

/// The principal EventBridge delivers to a resource-policy-governed target as: "for Lambda
/// functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on resource-based
/// policies" (`PutTargets`), so a queue that has granted it nothing receives nothing.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The four services this question spans, over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// One EventBridge call that must answer 200.
async fn events(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let host = format!("events.{REGION}.amazonaws.com");
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/events/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=0000"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header("authorization", authorization)
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
        "{operation} must answer 200 from EventBridge, got {status} with {parsed}"
    );
    parsed
}

/// The ARN of the bus `name`, which is the resource a publication onto it is taken on.
fn bus_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/{name}")
}

/// A queue called `name` carrying the grant a rule's target needs; answers its url and the
/// ARN a rule names it by.
async fn given_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = string(&created, "QueueUrl");
    let attributes = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("expected the QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        router,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// The custom bus, and a rule on EACH bus a row may publish onto, every one of them
/// delivering what it matches to the one queue this suite reads; answers that queue's url.
///
/// Both buses are watched, so a publication the role may not make shows up as an envelope
/// wherever it went rather than as the absence of one on the bus the row named.
async fn given_buses_and_rules(router: &MultiServiceRouter) -> String {
    let (queue_url, queue_arn) = given_queue(router, "publication-target").await;
    events(router, "CreateEventBus", &json!({ "Name": CUSTOM_BUS })).await;
    for (rule, bus) in [("watch-default", DEFAULT_BUS), ("watch-custom", CUSTOM_BUS)] {
        events(
            router,
            "PutRule",
            &json!({
                "Name": rule,
                "EventBusName": bus,
                "State": "ENABLED",
                "EventPattern": json!({
                    "source": [SOURCE],
                    "detail-type": [DETAIL_TYPE],
                })
                .to_string(),
            }),
        )
        .await;
        let wired = events(
            router,
            "PutTargets",
            &json!({
                "Rule": rule,
                "EventBusName": bus,
                "Targets": [{ "Id": "the-queue", "Arn": &queue_arn }],
            }),
        )
        .await;
        assert_eq!(
            wired.get("FailedEntryCount").and_then(Value::as_i64),
            Some(0),
            "the rule on {bus:?} must accept its queue target, got {wired}"
        );
    }
    queue_url
}

/// The envelopes waiting on the queue at `url` right now, taken out of it.
async fn drained(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "WaitTimeSeconds": 0 }),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            let body = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(body)
                .unwrap_or_else(|error| panic!("a delivered body is a JSON envelope: {body} ({error})"))
        })
        .collect()
}

/// The ONE envelope the queue at `url` is delivered, looked for until it arrives.
async fn delivered(router: &MultiServiceRouter, case: &str, url: &str) -> Value {
    let began = Instant::now();
    let mut held: Vec<Value> = Vec::new();
    loop {
        held.extend(drained(router, url).await);
        if let [only] = held.as_slice() {
            return only.clone();
        }
        assert!(
            held.is_empty(),
            "case {case:?}: one publication reaches the matching rule once, got {held:?}"
        );
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: the publication must reach the matching rule within {PATIENCE:?}, \
             nothing arrived"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// The entries one row publishes: `bus` is the bus each of them names, or `None` for an entry
/// that omits the member and so names the default bus, and `text` is set for the AWS SDK
/// family, whose `Detail` is the JSON TEXT of the document (`PutEventsRequestEntry$Detail` is
/// a String member).
fn entry(marker: &str, bus: Option<&str>, text: bool) -> Value {
    let detail = json!({ ROW_MEMBER: marker });
    let mut written = json!({
        "Source": SOURCE,
        "DetailType": DETAIL_TYPE,
        "Detail": match text {
            true => Value::String(detail.to_string()),
            false => detail,
        },
    });
    if let Some(bus) = bus {
        written["EventBusName"] = Value::String(bus.to_string());
    }
    written
}

/// One row of the refused table: the `Resource` the `Task` names, the entries it publishes,
/// which role runs it, and the bus the refusal must be about.
struct Refused {
    case: &'static str,
    resource: &'static str,
    entries: Value,
    /// Set for a row run by the role granted the DEFAULT bus and nothing else, which is what
    /// makes a publication onto the custom bus a refusal of its own.
    granted_default: bool,
    /// The name the client this row's family is served by gives the service.
    service: &'static str,
    /// The bus the role may not publish onto, which is the resource the refusal names.
    refused_bus: &'static str,
}

/// A publication whose bus the execution role is not granted is NOT made: the task fails with
/// EventBridge's own access-denied refusal, named after that service's base SDK exception, and
/// no entry of the batch reaches any bus.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_publication_the_execution_role_is_not_granted_is_not_made() {
    let router = emulator();
    let queue_url = given_buses_and_rules(&router).await;
    // One role carrying no grant about publishing at all, and one granted the DEFAULT bus
    // alone: a grant is per bus, so the second role is refused exactly on the buses its
    // statement does not name.
    let denied_role = role(
        &router,
        "PublicationDeniedRole",
        &json!({ "Version": "2012-10-17", "Statement": [logs_only()] }),
    )
    .await;
    let default_only_role = role(
        &router,
        "PublicationDefaultBusRole",
        &json!({
            "Version": "2012-10-17",
            "Statement": [logs_only(), allow(PUT_EVENTS_ACTION, &bus_arn(DEFAULT_BUS))],
        }),
    )
    .await;

    let rows = [
        Refused {
            case: "the default bus, which the entry names by omitting the member",
            resource: "arn:aws:states:::events:putEvents",
            entries: json!([entry("denied-default", None, false)]),
            granted_default: false,
            service: OPTIMIZED_SERVICE,
            refused_bus: DEFAULT_BUS,
        },
        Refused {
            // The other family makes the same call on the same bus, so the refusal must
            // hold through it too — reported by the client THAT family is served by.
            case: "the same publication through the AWS SDK integration",
            resource: "arn:aws:states:::aws-sdk:eventbridge:putEvents",
            entries: json!([entry("denied-sdk", None, true)]),
            granted_default: false,
            service: SDK_SERVICE,
            refused_bus: DEFAULT_BUS,
        },
        Refused {
            case: "a custom bus named by name, under a role granted the default bus alone",
            resource: "arn:aws:states:::events:putEvents",
            entries: json!([entry("denied-custom-by-name", Some(CUSTOM_BUS), false)]),
            granted_default: true,
            service: OPTIMIZED_SERVICE,
            refused_bus: CUSTOM_BUS,
        },
        Refused {
            case: "the same bus named by its ARN, which names the same bus",
            resource: "arn:aws:states:::events:putEvents",
            entries: json!([entry(
                "denied-custom-by-arn",
                Some(&bus_arn(CUSTOM_BUS)),
                false,
            )]),
            granted_default: true,
            service: OPTIMIZED_SERVICE,
            refused_bus: CUSTOM_BUS,
        },
        Refused {
            case: "a batch whose second entry names a bus the role was not granted",
            resource: "arn:aws:states:::events:putEvents",
            entries: json!([
                entry("denied-batch-granted-entry", None, false),
                entry("denied-batch-refused-entry", Some(CUSTOM_BUS), false),
            ]),
            granted_default: true,
            service: OPTIMIZED_SERVICE,
            refused_bus: CUSTOM_BUS,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let role_arn = match row.granted_default {
            true => &default_only_role,
            false => &denied_role,
        };
        let machine_arn = machine(
            &router,
            &format!("publication-denied-{index}"),
            row.resource,
            &json!({ "Entries": row.entries }),
            role_arn,
        )
        .await;
        let (execution_arn, described) = ran(&router, &machine_arn, case).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some(FAILED),
            "case {case:?}: expected {FAILED}, got {described}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some(REFUSAL_ERROR),
            "case {case:?}: expected {REFUSAL_ERROR:?}, got {described}"
        );
        let details = task_failure(&router, &execution_arn, case).await;
        let cause = string(&details, "cause");
        // The whole sentence, principal first: the SESSION of the role the call would have
        // been made as, then the action and the bus the entry named.
        let principal = session_principal(role_arn, &cause);
        let refused = bus_arn(row.refused_bus);
        let opens = format!(
            "User: {principal} is not authorized to perform: {PUT_EVENTS_ACTION} on resource: \
             {refused} "
        );
        assert!(
            cause.starts_with(&opens),
            "case {case:?}: expected the cause to open {opens:?}, got {cause:?}"
        );
        assert!(
            cause.contains(NO_IDENTITY_GRANT),
            "case {case:?}: expected the cause to name why, got {cause:?}"
        );
        // The metadata the cause closes with is the one the client this row's family is
        // served by writes: the service as that client names it, the status EventBridge's
        // front door answers with and the code its answer carried.
        let closes = refusal_metadata(row.resource, row.service, REFUSAL_CODE, REFUSAL_STATUS);
        let reported = masked(&cause);
        assert!(
            reported.ends_with(&closes),
            "case {case:?}: expected the cause to close {closes:?}, got {reported:?}"
        );
        // Nothing was published: neither bus matched the entry the role may not publish,
        // and a batch is refused whole, so the entry on the bus the role IS granted did not
        // reach it either.
        let waiting = drained(&router, &queue_url).await;
        assert!(
            waiting.is_empty(),
            "case {case:?}: expected no envelope on either bus, got {waiting:?}"
        );
    }
}

/// One row of the granted table: the `Resource`, the entries, the bus the entry names and the
/// marker its `Detail` carries.
struct Granted {
    case: &'static str,
    resource: &'static str,
    entries: Value,
    marker: &'static str,
    /// Set for the row run by the role whose grant names EVERY resource rather than the bus,
    /// which covers the bus the entry names.
    every_resource: bool,
}

/// A publication whose bus the execution role IS granted is made: the task reports
/// EventBridge's own answer and the entry reaches the rules of the bus it named.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_publication_the_execution_role_is_granted_is_made() {
    let router = emulator();
    let queue_url = given_buses_and_rules(&router).await;
    let both_buses_role = role(
        &router,
        "PublicationGrantedRole",
        &json!({
            "Version": "2012-10-17",
            "Statement": [
                logs_only(),
                allow(PUT_EVENTS_ACTION, &bus_arn(DEFAULT_BUS)),
                allow(PUT_EVENTS_ACTION, &bus_arn(CUSTOM_BUS)),
            ],
        }),
    )
    .await;
    let every_resource_role = role(
        &router,
        "PublicationWildcardRole",
        &json!({
            "Version": "2012-10-17",
            "Statement": [logs_only(), allow(PUT_EVENTS_ACTION, "*")],
        }),
    )
    .await;

    let rows = [
        Granted {
            case: "the default bus, under a grant naming that bus",
            resource: "arn:aws:states:::events:putEvents",
            entries: json!([entry("granted-default", None, false)]),
            marker: "granted-default",
            every_resource: false,
        },
        Granted {
            case: "a custom bus, under a grant naming that bus",
            resource: "arn:aws:states:::events:putEvents",
            entries: json!([entry("granted-custom", Some(CUSTOM_BUS), false)]),
            marker: "granted-custom",
            every_resource: false,
        },
        Granted {
            case: "the same custom bus, under a grant naming every resource",
            resource: "arn:aws:states:::events:putEvents",
            entries: json!([entry("granted-wildcard", Some(CUSTOM_BUS), false)]),
            marker: "granted-wildcard",
            every_resource: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let role_arn = match row.every_resource {
            true => &every_resource_role,
            false => &both_buses_role,
        };
        let machine_arn = machine(
            &router,
            &format!("publication-granted-{index}"),
            row.resource,
            &json!({ "Entries": row.entries }),
            role_arn,
        )
        .await;
        let (_execution_arn, described) = ran(&router, &machine_arn, case).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some(SUCCEEDED),
            "case {case:?}: expected {SUCCEEDED}, got {described}"
        );
        // What the task reports is EventBridge's own answer: every entry taken, each with
        // the id that service minted for it.
        let output: Value = serde_json::from_str(&string(&described, "output"))
            .unwrap_or_else(|error| panic!("case {case:?}: the output is a document ({error})"));
        assert_eq!(
            output.get("FailedEntryCount").and_then(Value::as_i64),
            Some(0),
            "case {case:?}: expected every entry taken, got {output}"
        );
        let minted = output
            .pointer("/Entries/0/EventId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            !minted.is_empty(),
            "case {case:?}: expected the id EventBridge minted for the entry, got {output}"
        );
        // And the entry really reached the bus: the rule of the bus it named matched it and
        // delivered the envelope to that rule's queue target.
        let envelope = delivered(&router, case, &queue_url).await;
        assert_eq!(
            envelope.pointer(&format!("/detail/{ROW_MEMBER}")),
            Some(&Value::String(row.marker.to_string())),
            "case {case:?}: expected the envelope this row published, got {envelope}"
        );
        assert_eq!(
            envelope.get("source").and_then(Value::as_str),
            Some(SOURCE),
            "case {case:?}: expected the source the entry carried, got {envelope}"
        );
    }
}
