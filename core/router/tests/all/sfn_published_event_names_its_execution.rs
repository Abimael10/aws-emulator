// SPDX-License-Identifier: CC0-1.0

//! What a `Task` state's EVENT PUBLICATION names as the resources its events concern, and
//! what a publication that rejected one of its entries makes of the task — driven over
//! three services' own wire protocols with Step Functions, EventBridge and Amazon SQS
//! mounted on ONE engine, so the entries the state built are really published, really
//! routed by a rule, and really delivered to that rule's queue target.
//!
//! RULE: a `Task` state's EventBridge `PutEvents` integration call is made BY Step
//! Functions as the execution's work, so every entry of it names the two resources that
//! publication is attributable to — the calling execution's ARN and its state machine's —
//! among the `Resources` the entry carries, added to any the state wrote there itself and
//! each named once; and a publication the service served but which REJECTED an entry
//! (`FailedEntryCount` above zero) did not do the task's work, so it fails the task under
//! `EventBridge.FailedEntry`, carrying what the publication answered, instead of being
//! reported as that task's result.
//!
//! An ENTRY's own property is what EventBridge rejects, and it rejects it on that entry's
//! own row of a served answer: the entries of the same batch it took are published and
//! delivered as any other, whether the batch held one such entry or nothing but such
//! entries.
//!
//! `Resources` is "Amazon Web Services resources, identified by Amazon Resource Name (ARN),
//! which the event primarily concerns. Any number, including zero, may be present"
//! (`smithy_query(query="shape",shape="PutEventsRequestEntry",model_path="services/
//! service-com-amazonaws-eventbridge-awsevents/smithy-model/
//! com-amazonaws-eventbridge-awsevents.json")`), which is what lets a consumer attribute a
//! delivered event to the run that produced it and what a rule pattern constraining
//! `resources` is matched against.
//!
//! The rows differ along what the rule generalises over: the `Resource` form that names the
//! publication (the optimized `events:putEvents` integration and the AWS SDK
//! `aws-sdk:eventbridge:putEvents` one), what the entry writes for its own `Resources` (no
//! such member, an empty list, an ARN of its own, and the execution's own ARN written by
//! the state itself), and how many entries the one batch carries (one, and two differing in
//! what each names).
//!
//! Every expected value is computed here from what the control plane answered — the state
//! machine's ARN out of `CreateStateMachine`, the execution's out of `StartExecution` —
//! never read back out of the envelope being checked.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// `tests/all/` is one crate root, so the member reader and the Region every request below
// is scoped to are borrowed from the Step Functions harness rather than copied.
use crate::sfn_task_target_authorization::{string, REGION};

/// The role every state machine below runs as. No IAM service is mounted beside these
/// three, so there are no identity-based documents to decide the publication against: what
/// is under test is what the publication CARRIES, which
/// `all/sfn_task_target_authorization.rs` owns the authorization half of.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/StepFunctionsPublisher";

/// The `detail-type` every entry below carries, and the constant inside each `Detail` so a
/// row's rule matches that row's publication and nothing else on the bus.
const DETAIL_TYPE: &str = "task.published";
const NONCE: &str = "sfn-published-event";

/// An ARN of the caller's OWN, for the entries that name a resource themselves: a bucket the
/// event is about, which the publication must carry through beside what this service adds.
const OWN_RESOURCE_ARN: &str = "arn:aws:s3:::the-bucket-the-event-is-about";

/// The terminal statuses an execution below reaches, and the one it sits in while its task
/// is still running (`ExecutionStatus`).
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";
const RUNNING: &str = "RUNNING";

/// The error name a `Task` whose publication rejected an entry fails with.
const FAILED_ENTRY_ERROR: &str = "EventBridge.FailedEntry";

/// The per-entry codes EventBridge reports a rejected entry under, in its own words:
/// "MalformedDetail — the JSON provided is not valid" and "InvalidArgument — a specified
/// parameter is not valid"
/// (`smithy_query(query="doc",shape="PutEventsResultEntry",member="ErrorCode",model_path=
/// "services/service-com-amazonaws-eventbridge-awsevents/smithy-model/
/// com-amazonaws-eventbridge-awsevents.json")`).
const MALFORMED_DETAIL: &str = "MalformedDetail";
/// See [`MALFORMED_DETAIL`].
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// The history event a failed task writes
/// (`smithy_query(query="shape",shape="HistoryEventType")`).
const TASK_FAILED: &str = "TaskFailed";

/// The principal EventBridge delivers to a resource-policy-governed target as: "for Lambda
/// functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on resource-based
/// policies" (`PutTargets`), so a queue that has granted it nothing receives nothing.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// How long one poll waits before looking again, and how long this suite waits for work the
/// clock and the three services owe: generous, so a loaded host cannot decide the outcome.
const POLL: Duration = Duration::from_millis(25);
/// See [`POLL`].
const PATIENCE: Duration = Duration::from_secs(120);

/// The three services this question spans, over ONE shared engine: the publication is a
/// Step Functions task's call onto an EventBridge bus, and what the rule matched is
/// delivered to an Amazon SQS queue, which is where this suite reads the envelope.
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
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// One `awsJson` call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=0000"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", &host)
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
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
    (status, parsed)
}

/// One call of a service that must answer 200.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        signing_name,
        target_prefix,
        json_version,
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One Step Functions call that must answer 200.
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    served(
        router,
        case,
        "states",
        "AWSStepFunctions",
        "1.0",
        operation,
        body,
    )
    .await
}

/// One EventBridge call that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    served(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    served(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// A queue called `name` carrying the grant a rule's target needs; answers its url and the
/// ARN a rule names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = string(&created, "QueueUrl");
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: GetQueueAttributes answers a QueueArn, got {attributes}"))
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
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// A rule on the default bus matching `source` and [`DETAIL_TYPE`], with its own queue as
/// its one target; answers the url the events it matches arrive on.
async fn given_rule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    source: &str,
) -> String {
    let (url, queue_arn) = given_queue(router, case, &format!("{name}-queue")).await;
    events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "State": "ENABLED",
            "EventPattern": json!({
                "source": [source],
                "detail-type": [DETAIL_TYPE],
                "detail": { "nonce": [NONCE] },
            })
            .to_string(),
        }),
    )
    .await;
    let wired = events(
        router,
        case,
        "PutTargets",
        &json!({ "Rule": name, "Targets": [{ "Id": "the-queue", "Arn": queue_arn }] }),
    )
    .await;
    assert_eq!(
        wired.get("FailedEntryCount").and_then(Value::as_i64),
        Some(0),
        "case {case:?}: the rule's queue target must be accepted, got {wired}"
    );
    url
}

/// A state machine whose one `Task` state publishes `entries` through `resource`; answers
/// its ARN.
async fn given_machine(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    resource: &str,
    entries: &[Value],
) -> String {
    let definition = json!({
        "StartAt": "Publish",
        "States": {
            "Publish": {
                "Type": "Task",
                "Resource": resource,
                "Parameters": { "Entries": entries },
                "End": true,
            },
        },
    })
    .to_string();
    let created = sfn(
        router,
        case,
        "CreateStateMachine",
        &json!({ "name": name, "definition": definition, "roleArn": ROLE_ARN }),
    )
    .await;
    string(&created, "stateMachineArn")
}

/// Start one execution of `machine` and report it once it has left [`RUNNING`]; answers its
/// ARN and what `DescribeExecution` then reported.
async fn ran(router: &MultiServiceRouter, case: &str, machine: &str) -> (String, Value) {
    let started = sfn(
        router,
        case,
        "StartExecution",
        &json!({ "stateMachineArn": machine, "name": "only" }),
    )
    .await;
    let execution = string(&started, "executionArn");
    let began = Instant::now();
    loop {
        let described = sfn(
            router,
            case,
            "DescribeExecution",
            &json!({ "executionArn": &execution }),
        )
        .await;
        if described.get("status").and_then(Value::as_str) != Some(RUNNING) {
            return (execution, described);
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: the execution must reach a terminal status within {PATIENCE:?}, \
             last saw {described}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// The `want` envelopes the queue at `url` is delivered, looked for until they have all
/// arrived; a message is taken out of the queue once read, so they accumulate across polls.
async fn delivered(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    want: usize,
) -> Vec<Value> {
    let began = Instant::now();
    let mut held: Vec<Value> = Vec::new();
    while held.len() < want {
        let received = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "WaitTimeSeconds": 0 }),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default();
            held.push(serde_json::from_str::<Value>(body).unwrap_or_else(|error| {
                panic!("case {case:?}: a delivered body must be a JSON envelope: {body} ({error})")
            }));
        }
        assert!(
            held.len() <= want,
            "case {case:?}: expected {want} delivered envelope(s), got {held:?}"
        );
        if held.len() < want {
            assert!(
                began.elapsed() < PATIENCE,
                "case {case:?}: expected {want} delivered envelope(s) within {PATIENCE:?}, got \
                 {held:?}"
            );
            ::tokio::time::sleep(POLL).await;
        }
    }
    held
}

/// The resources a delivered envelope names, as a set: the member is a list, and what the
/// rule is about is which ARNs are in it.
fn resources_of(case: &str, envelope: &Value) -> BTreeSet<String> {
    let named = envelope
        .get("resources")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a delivered envelope names its resources, got {envelope}")
        });
    let set: BTreeSet<String> = named
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every resource is an ARN string, got {envelope}")
                })
                .to_string()
        })
        .collect();
    assert_eq!(
        set.len(),
        named.len(),
        "case {case:?}: no resource may be named twice, got {envelope}"
    );
    set
}

/// What the entry of a row writes for its OWN `Resources`.
#[derive(Clone, Copy, Debug)]
enum Named {
    /// No such member at all, which is what most callers write.
    Absent,
    /// The member, carrying no ARN: "any number, including zero, may be present".
    Empty,
    /// One ARN of the caller's own ([`OWN_RESOURCE_ARN`]), which must be carried through.
    Own,
    /// The execution's OWN ARN, written by the state itself out of the context object, so
    /// what this service adds is already there.
    TheExecution,
}

impl Named {
    /// The `Resources` this writes into an entry, as the state's `Parameters` express it.
    fn written(self) -> Option<(&'static str, Value)> {
        match self {
            Self::Absent => None,
            Self::Empty => Some(("Resources", json!([]))),
            Self::Own => Some(("Resources", json!([OWN_RESOURCE_ARN]))),
            // A payload-template field ending in `.$` is an expression: the context
            // object's `Execution.Id` is the ARN of the execution being run.
            Self::TheExecution => Some((
                "Resources.$",
                Value::String("States.Array($$.Execution.Id)".to_string()),
            )),
        }
    }

    /// The ARNs the entry itself contributes to the delivered envelope.
    fn contributed(self, execution: &str) -> Vec<String> {
        match self {
            Self::Absent | Self::Empty => Vec::new(),
            Self::Own => vec![OWN_RESOURCE_ARN.to_string()],
            Self::TheExecution => vec![execution.to_string()],
        }
    }
}

/// One row: the `Resource` form naming the publication, whether that form is the AWS SDK
/// one (whose `Detail` is the JSON TEXT of the document, the member's own modelled type),
/// and what each entry of the one batch writes for its own `Resources`.
struct Row {
    case: &'static str,
    resource: &'static str,
    aws_sdk: bool,
    named: &'static [Named],
}

const ROWS: &[Row] = &[
    Row {
        case: "the optimized integration, an entry naming no resources of its own",
        resource: "arn:aws:states:::events:putEvents",
        aws_sdk: false,
        named: &[Named::Absent],
    },
    Row {
        case: "an entry whose own Resources is the empty list",
        resource: "arn:aws:states:::events:putEvents",
        aws_sdk: false,
        named: &[Named::Empty],
    },
    Row {
        case: "an entry naming a resource of its own, which is carried through",
        resource: "arn:aws:states:::events:putEvents",
        aws_sdk: false,
        named: &[Named::Own],
    },
    Row {
        case: "an entry that names the execution itself, which is not named twice",
        resource: "arn:aws:states:::events:putEvents",
        aws_sdk: false,
        named: &[Named::TheExecution],
    },
    Row {
        case: "the AWS SDK family of the same publication",
        resource: "arn:aws:states:::aws-sdk:eventbridge:putEvents",
        aws_sdk: true,
        named: &[Named::Absent],
    },
    Row {
        case: "a batch of two entries, differing in what each names itself",
        resource: "arn:aws:states:::events:putEvents",
        aws_sdk: false,
        named: &[Named::Absent, Named::Own],
    },
];

/// The `index`-th entry of `row`'s batch, published under `source`.
fn entry(row: &Row, source: &str, index: usize, named: Named) -> Value {
    let detail = json!({ "nonce": NONCE, "entry": index });
    let mut entry = json!({ "Source": source, "DetailType": DETAIL_TYPE });
    // The optimized integration takes the detail as the DOCUMENT the event carries; the
    // AWS SDK family takes the operation's own member, which is a string.
    entry["Detail"] = match row.aws_sdk {
        true => Value::String(detail.to_string()),
        false => detail,
    };
    if let Some((member, value)) = named.written() {
        entry[member] = value;
    }
    entry
}

/// Every row: each entry the `Task` published is delivered naming the execution that
/// published it and that execution's state machine, beside whatever the entry named itself,
/// and the execution succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_published_event_names_the_execution_and_state_machine_that_published_it() {
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let source = format!("sim.sfn.published.{index}");
        let queue_url = given_rule(&router, case, &format!("published-rule-{index}"), &source).await;
        let entries: Vec<Value> = row
            .named
            .iter()
            .enumerate()
            .map(|(position, named)| entry(row, &source, position, *named))
            .collect();
        let machine = given_machine(
            &router,
            case,
            &format!("published-machine-{index}"),
            row.resource,
            &entries,
        )
        .await;
        let (execution, described) = ran(&router, case, &machine).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some(SUCCEEDED),
            "case {case:?}: a publication every entry of which was taken succeeds, got \
             {described}"
        );

        let envelopes = delivered(&router, case, &queue_url, row.named.len()).await;
        for (position, named) in row.named.iter().enumerate() {
            let envelope = envelopes
                .iter()
                .find(|envelope| {
                    envelope.pointer("/detail/entry").and_then(Value::as_u64)
                        == u64::try_from(position).ok()
                })
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: entry {position} of the batch must be delivered, got \
                         {envelopes:?}"
                    )
                });
            let mut expected: BTreeSet<String> =
                [execution.clone(), machine.clone()].into_iter().collect();
            expected.extend(named.contributed(&execution));
            let actual = resources_of(case, envelope);
            assert_eq!(
                actual, expected,
                "case {case:?}: entry {position} ({named:?}) must name exactly {expected:?}, got \
                 {actual:?} in {envelope}"
            );
        }
    }
}

/// One rejection row: the batch a `Task` publishes, how many of its entries EventBridge
/// takes (so how many envelopes the rule's queue is delivered), and the per-entry
/// `ErrorCode` the publication reports for the one it will not take.
struct Rejected {
    case: &'static str,
    /// The entries, in the request's own order; the taken ones carry `"entry": <index>`
    /// in their `Detail`, as [`ROWS`] above do.
    entries: fn(&str) -> Vec<Value>,
    taken: usize,
    code: &'static str,
}

/// The entries of each rejection row.
///
/// The classes span what makes EventBridge refuse ONE entry rather than the request: a
/// `Detail` that is not a JSON object ("a valid JSON object. There is no other schema
/// imposed"), an entry that does not provide one of the three properties an event needs
/// ("`Detail`, `DetailType`, and `Source` are required […] EventBridge fails that entry"),
/// and a `DetailType` past the maximum its own documentation states — each beside a good
/// entry and each alone in its batch, since what the rule generalises over is the entry,
/// not the batch around it.
const REJECTIONS: &[Rejected] = &[
    Rejected {
        case: "a good entry beside one whose Detail is not a JSON object",
        entries: |source| {
            vec![
                taken_entry(source, 0),
                json!({ "Source": source, "DetailType": DETAIL_TYPE, "Detail": "not an object" }),
            ]
        },
        taken: 1,
        code: MALFORMED_DETAIL,
    },
    Rejected {
        case: "a batch whose only entry provides no Detail at all",
        entries: |source| vec![json!({ "Source": source, "DetailType": DETAIL_TYPE })],
        taken: 0,
        code: INVALID_ARGUMENT,
    },
    Rejected {
        case: "a good entry beside one that provides no DetailType",
        entries: |source| {
            vec![
                taken_entry(source, 0),
                json!({ "Source": source, "Detail": { "nonce": NONCE } }),
            ]
        },
        taken: 1,
        code: INVALID_ARGUMENT,
    },
    Rejected {
        case: "a batch whose only entry carries a DetailType past 128 characters",
        entries: |source| {
            vec![json!({
                "Source": source,
                // "Free-form string, with a maximum of 128 characters"
                // (`PutEventsRequestEntry$DetailType`): one past it names no event the
                // service will take.
                "DetailType": "d".repeat(129),
                "Detail": { "nonce": NONCE },
            })]
        },
        taken: 0,
        code: INVALID_ARGUMENT,
    },
];

/// One entry EventBridge takes, carrying its position in the batch so the envelope it is
/// delivered as can be told from its siblings'.
fn taken_entry(source: &str, index: usize) -> Value {
    json!({
        "Source": source,
        "DetailType": DETAIL_TYPE,
        "Detail": { "nonce": NONCE, "entry": index },
    })
}

/// Every rejection row: the entries the publication TOOK are published — delivered naming
/// the execution and its state machine, like any other — and the task fails under
/// [`FAILED_ENTRY_ERROR`], its cause carrying what the publication answered: the count of
/// rejected entries and each rejected row's own code.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rejected_entry_fails_the_task_that_published_the_batch() {
    let router = emulator();

    for (index, row) in REJECTIONS.iter().enumerate() {
        let case = row.case;
        let source = format!("sim.sfn.rejected.{index}");
        let queue_url = given_rule(&router, case, &format!("rejected-rule-{index}"), &source).await;
        let entries = (row.entries)(&source);
        let rejected = entries.len() - row.taken;
        let machine = given_machine(
            &router,
            case,
            &format!("rejected-machine-{index}"),
            "arn:aws:states:::events:putEvents",
            &entries,
        )
        .await;
        let (execution, described) = ran(&router, case, &machine).await;

        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some(FAILED), Some(FAILED_ENTRY_ERROR)),
            "case {case:?}: expected {FAILED} under {FAILED_ENTRY_ERROR}, got {described}"
        );

        // The cause is what the publication answered, so a handler reads the rejected
        // entries out of it: the count, and the code on the row of each entry not taken.
        let text = described.get("cause").and_then(Value::as_str).unwrap_or_else(|| {
            panic!("case {case:?}: a failed execution carries a cause, got {described}")
        });
        let answered: Value = serde_json::from_str(text).unwrap_or_else(|error| {
            panic!("case {case:?}: the cause is the publication's own answer, got {error} in {text}")
        });
        assert_eq!(
            answered.get("FailedEntryCount").and_then(Value::as_u64),
            u64::try_from(rejected).ok(),
            "case {case:?}: expected {rejected} rejected entr(ies) in the cause, got {answered}"
        );
        let rows = answered
            .get("Entries")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        assert_eq!(
            rows.len(),
            entries.len(),
            "case {case:?}: expected one row per entry in the cause, got {answered}"
        );
        for row_of in rows.iter().skip(row.taken) {
            assert_eq!(
                row_of.get("ErrorCode").and_then(Value::as_str),
                Some(row.code),
                "case {case:?}: expected the rejected row to report {}, got {answered}",
                row.code
            );
        }

        // The same verdict on the task itself, in the history the execution keeps.
        let history = sfn(
            &router,
            case,
            "GetExecutionHistory",
            &json!({ "executionArn": &execution, "maxResults": 100 }),
        )
        .await;
        let failures: Vec<&Value> = history
            .get("events")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|event| event.get("type").and_then(Value::as_str) == Some(TASK_FAILED))
            .collect();
        assert_eq!(
            failures.len(),
            1,
            "case {case:?}: expected exactly one {TASK_FAILED}, got {history}"
        );
        assert_eq!(
            failures[0]
                .pointer("/taskFailedEventDetails/error")
                .and_then(Value::as_str),
            Some(FAILED_ENTRY_ERROR),
            "case {case:?}: the failed task names {FAILED_ENTRY_ERROR}, got {:?}",
            failures[0]
        );

        // And every entry the publication DID take reached the rule's target, attributed
        // to the execution that published it exactly as an unrejected batch's entries are.
        let envelopes = delivered(&router, case, &queue_url, row.taken).await;
        let expected: BTreeSet<String> =
            [execution.clone(), machine.clone()].into_iter().collect();
        for envelope in &envelopes {
            let actual = resources_of(case, envelope);
            assert_eq!(
                actual, expected,
                "case {case:?}: a taken entry must name exactly {expected:?}, got {actual:?} in \
                 {envelope}"
            );
        }
    }
}
