// SPDX-License-Identifier: CC0-1.0

//! A write to Parameter Store reaches an EventBridge rule's Amazon SQS target.
//!
//! The RULE pinned here: **every write that changes which parameter versions an account
//! holds — a put that CREATES a parameter, a put that stores another version of one, and a
//! delete that REMOVES one — is published once onto the account's DEFAULT EventBridge bus as
//! `source` `aws.ssm` / `detail-type` `Parameter Store Change`, whose `detail` names the
//! parameter (`name`), its `type`, the `operation` the write performed (`Create`, `Update`,
//! `Delete`) and its `description` where it carries one, and which names that parameter's own
//! ARN in `resources`; EventBridge routes the envelope by its CONTENT, so a rule whose
//! `EventPattern` matches hands it UNCHANGED to every one of that rule's targets, and a queue
//! target receives it where that QUEUE's own resource policy admits `sqs:SendMessage` to the
//! `events.amazonaws.com` principal ("for Lambda functions, Amazon SNS topics and Amazon SQS
//! queues, EventBridge relies on resource-based policies", `PutTargets`). A call that changed
//! no parameter — a refused write, a read, a delete of a name the Region holds none of —
//! publishes nothing.**
//!
//! Source for the envelope: the Parameter Store EventBridge events, which carry
//! `"source": "aws.ssm"` with `"detail-type": "Parameter Store Change"` and a `detail` of
//! `operation` / `name` / `type` / `description` for a parameter that is created, updated or
//! deleted. The Systems Manager Smithy model describes the control plane, not the bus
//! (`smithy_query(query="operation", operation="PutParameter")` models only the synchronous
//! response), so every asserted `detail` member is asserted against what the SAME write's own
//! control-plane surface reports for it — the `Type` and `Description` the call carried, and
//! the `ARN` `GetParameter` reports for the parameter — never against a value this suite put
//! there alone.
//!
//! Three crates have to agree for the rule to hold — Systems Manager publishes the write,
//! EventBridge matches the envelope against each installed pattern and hands it over, and
//! Amazon SQS holds what was handed to it — so the suite lives here rather than in any one of
//! them.
//!
//! The rows differ along exactly what the rule generalises over: the parameter's `Type`
//! (`String`, `StringList`, `SecureString`, whose value is sealed under a KMS key before it is
//! stored); the SHAPE of its name (flat, one level of hierarchy, several); whether it carries
//! a `description`, and whether an update adds, changes or leaves one; and WHICH write is
//! performed (a create, one update, two updates, a delete). Beside the table are the three
//! cases a single row cannot carry: a BATCH delete, which owes one envelope per parameter it
//! really removed; the calls that change nothing, which owe none; and which installed
//! `EventPattern` is owed a change, read on five patterns at once.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The account every generated emulator presents, which an envelope's `account` names.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The Region every request below is signed for, and which an envelope's `region` names.
const REGION: &str = "us-east-1";

/// The `source` and `detail-type` Parameter Store publishes a write under, spelled out here
/// rather than imported from the service crate, so a wrong constant in the implementation
/// cannot make this suite pass.
const EVENT_SOURCE: &str = "aws.ssm";
const PARAMETER_STORE_CHANGE: &str = "Parameter Store Change";

/// The three writes `detail.operation` reports.
const CREATE: &str = "Create";
const UPDATE: &str = "Update";
const DELETE: &str = "Delete";

/// How long a poll loop looks for an envelope before failing the case.
///
/// Generous on purpose: the loop waits for the ENVELOPE, looked for again until it is there,
/// and this deadline only bounds one that is never published at all — so a loaded or a slow
/// host still sees one that is.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// Systems Manager, EventBridge, Amazon SQS and KMS over ONE engine, as
/// `core/router/src/lib.rs` mounts them: the publication reaches EventBridge through the
/// in-process callback, a rule on that bus delivers to the queue this suite reads, and a
/// `SecureString`'s value is sealed under a key KMS holds.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Nothing here signs: the `Authorization` header carries only the credential scope,
    // which is where a service reads the Region from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
    )
}

/// One JSON-protocol call at `signing_name`'s endpoint, as its status and parsed body
/// (`Value::Null` for an empty or unparseable body).
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Systems Manager call (`awsJson1_1`, `AmazonSSM.<Operation>`), with whatever status it
/// answered.
async fn ssm_answered(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    call(
        router,
        "ssm",
        "application/x-amz-json-1.1",
        &format!("AmazonSSM.{operation}"),
        body,
    )
    .await
}

/// The same call, which must answer 200.
async fn ssm(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = ssm_answered(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Systems Manager {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`), which must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = call(
        router,
        "events",
        "application/x-amz-json-1.1",
        &format!("AWSEvents.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`), which must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = call(
        router,
        "sqs",
        "application/x-amz-json-1.0",
        &format!("AmazonSQS.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A queue called `name`, carrying the grant a rule's target needs — EventBridge makes the
/// target's `sqs:SendMessage` as its own service principal and relies on the queue's resource
/// policy for it, so a queue that has granted it nothing receives nothing. Answers the
/// queue's url and the ARN a rule names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
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

/// Install an ENABLED rule called `name` whose `EventPattern` is `pattern` on the default
/// bus, with the queue `target_arn` as its only target.
async fn given_rule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    pattern: &Value,
    target_arn: &str,
) {
    events(
        router,
        case,
        "PutRule",
        &json!({ "Name": name, "State": "ENABLED", "EventPattern": pattern.to_string() }),
    )
    .await;
    events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "Targets": [{ "Id": format!("{name}-target"), "Arn": target_arn }],
        }),
    )
    .await;
}

/// A queue carrying the rule `pattern`, as the url a case reads it through.
async fn given_rule_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    pattern: &Value,
) -> String {
    let (url, arn) = given_queue(router, case, name).await;
    given_rule(router, case, name, pattern, &arn).await;
    url
}

/// Every envelope waiting on the queue at `url`, oldest first.
///
/// Drained in several receives because one `ReceiveMessage` answers at most ten messages and a
/// received message is invisible afterwards, so the loop sees each message once. It stops on
/// the first empty receive, and after at most `MAX_RECEIVES` of them whatever the queue holds.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    /// How many `ReceiveMessage` calls one drain makes at most.
    const MAX_RECEIVES: usize = 4;
    let mut envelopes = Vec::new();
    for _ in 0..MAX_RECEIVES {
        let received = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            break;
        }
        for message in messages {
            let raw = message["Body"].as_str().unwrap_or_default();
            envelopes.push(serde_json::from_str::<Value>(raw).unwrap_or(Value::Null));
        }
    }
    envelopes
}

/// Poll the queue at `url` until it has handed over at least `wanted` envelopes, or until
/// [`POLL_DEADLINE`] passes.
///
/// The case waits for the ENVELOPES rather than for a span: one not delivered yet is looked
/// for again, and one that is never delivered fails the case at the deadline rather than
/// hanging it. A drain takes what it reports off the queue, so what each attempt collects is
/// kept.
async fn delivered_at_least(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut envelopes = Vec::new();
    loop {
        envelopes.extend(drain(router, case, url).await);
        if envelopes.len() >= wanted {
            return envelopes;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected at least {wanted} envelope(s) on {url:?} within \
             {POLL_DEADLINE:?}, got {envelopes:?}"
        );
        // Hand the runtime back, so anything the emulator is doing beside this task makes
        // progress between attempts rather than only when this one happens to block.
        ::tokio::task::yield_now().await;
    }
}

/// What ONE write of a parameter is expected to publish.
#[derive(Debug, Clone)]
struct Expected {
    operation: &'static str,
    name: String,
    type_: String,
    /// The description the parameter carries at that write, when it carries one.
    description: Option<String>,
    /// The parameter's ARN, as its own control-plane read reported it.
    arn: String,
}

/// Hold one envelope to the whole of what the rule says it carries.
fn assert_envelope(case: &str, envelope: &Value, owed: &Expected) {
    assert_eq!(
        envelope["source"],
        json!(EVENT_SOURCE),
        "case {case:?}: expected the source {EVENT_SOURCE:?}, got {envelope}"
    );
    assert_eq!(
        envelope["detail-type"],
        json!(PARAMETER_STORE_CHANGE),
        "case {case:?}: expected the detail-type {PARAMETER_STORE_CHANGE:?}, got {envelope}"
    );
    assert_eq!(
        envelope["account"],
        json!(ACCOUNT),
        "case {case:?}: expected the account {ACCOUNT:?}, got {envelope}"
    );
    assert_eq!(
        envelope["region"],
        json!(REGION),
        "case {case:?}: expected the region {REGION:?}, got {envelope}"
    );
    assert_eq!(
        envelope["resources"],
        json!([owed.arn]),
        "case {case:?}: expected the parameter's own ARN in resources, got {envelope}"
    );
    assert!(
        envelope["id"].as_str().is_some_and(|id| !id.is_empty()),
        "case {case:?}: expected the envelope to carry an id, got {envelope}"
    );
    assert!(
        envelope["time"].as_str().is_some_and(|at| !at.is_empty()),
        "case {case:?}: expected the envelope to carry a time, got {envelope}"
    );
    let mut detail = json!({
        "operation": owed.operation,
        "name": owed.name,
        "type": owed.type_,
    });
    if let Some(description) = &owed.description {
        detail["description"] = json!(description);
    }
    assert_eq!(
        envelope["detail"], detail,
        "case {case:?}: expected the detail {detail}, got {envelope}"
    );
}

/// One write a row performs: the members the put carries, or the delete of the parameter.
#[derive(Debug, Clone, Copy)]
enum Write {
    /// A put that creates the parameter, with this `Description` (`None` for a put that
    /// carries none).
    Create(Option<&'static str>),
    /// A put of another value with `Overwrite`, with this `Description` (`None` for a put
    /// that carries none, which inherits the description the parameter already holds).
    Update(&'static str, Option<&'static str>),
    /// The delete of the parameter.
    Delete,
}

/// One row of the table: the parameter a client writes, and the writes it is given.
struct Row {
    case: &'static str,
    /// The `Name` every write of the row addresses.
    name: &'static str,
    /// A `ParameterType` value, as the model's `ParameterType` enum lists them: `String`,
    /// `StringList`, `SecureString`.
    type_: &'static str,
    /// The `Value` the create stores.
    value: &'static str,
    writes: &'static [Write],
}

/// Every parameter write publishes the change it made, as `aws.ssm` /
/// `Parameter Store Change`, to the targets of a rule whose pattern matches it.
#[tokio::test]
async fn each_parameter_write_publishes_a_parameter_store_change() {
    let rows = [
        Row {
            case: "a flat String created with a description, then updated twice and deleted",
            name: "ParameterEventsFlat",
            type_: "String",
            value: "first",
            writes: &[
                Write::Create(Some("the flat one")),
                Write::Update("second", Some("the flat one, revised")),
                Write::Update("third", None),
                Write::Delete,
            ],
        },
        Row {
            case: "a hierarchical StringList created with no description, then described",
            name: "/parameter-events/list",
            type_: "StringList",
            value: "one,two",
            writes: &[
                Write::Create(None),
                Write::Update("one,two,three", Some("the list")),
                Write::Delete,
            ],
        },
        Row {
            case: "a deeply hierarchical SecureString sealed under the service's own key",
            name: "/parameter-events/deep/branch/secret",
            type_: "SecureString",
            value: "sealed-value",
            writes: &[
                Write::Create(Some("the sealed one")),
                Write::Update("sealed-again", None),
                Write::Delete,
            ],
        },
        Row {
            case: "a one-level String created and deleted, never updated",
            name: "/parameter-events/once",
            type_: "String",
            value: "only",
            writes: &[Write::Create(None), Write::Delete],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let router = build_router();
        // Owed EVERY envelope of this source, so a row can tell an envelope that was
        // withheld from one a pattern filtered out.
        let queue = given_rule_queue(
            &router,
            case,
            &format!("parameter-events-{index}"),
            &json!({ "source": [EVENT_SOURCE] }),
        )
        .await;

        let mut expected: Vec<Expected> = Vec::new();
        // The description the parameter carries right now: a put that states none inherits
        // the stored one, which is what the event then reports.
        let mut description: Option<String> = None;
        let mut arn = String::new();
        for write in row.writes {
            match write {
                Write::Create(stated) => {
                    let mut body = json!({
                        "Name": row.name,
                        "Value": row.value,
                        "Type": row.type_,
                    });
                    if let Some(stated) = stated {
                        body["Description"] = json!(stated);
                        description = Some((*stated).to_string());
                    }
                    ssm(&router, case, "PutParameter", &body).await;
                    // The ARN the event names is the parameter's own, which is what the
                    // control plane reports for it — read here rather than assembled.
                    let read = ssm(
                        &router,
                        case,
                        "GetParameter",
                        &json!({ "Name": row.name }),
                    )
                    .await;
                    arn = read["Parameter"]["ARN"]
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case:?}: expected an ARN in {read}"))
                        .to_string();
                    expected.push(Expected {
                        operation: CREATE,
                        name: row.name.to_string(),
                        type_: row.type_.to_string(),
                        description: description.clone(),
                        arn: arn.clone(),
                    });
                }
                Write::Update(value, stated) => {
                    let mut body = json!({
                        "Name": row.name,
                        "Value": value,
                        "Type": row.type_,
                        "Overwrite": true,
                    });
                    if let Some(stated) = stated {
                        body["Description"] = json!(stated);
                        description = Some((*stated).to_string());
                    }
                    ssm(&router, case, "PutParameter", &body).await;
                    expected.push(Expected {
                        operation: UPDATE,
                        name: row.name.to_string(),
                        type_: row.type_.to_string(),
                        description: description.clone(),
                        arn: arn.clone(),
                    });
                }
                Write::Delete => {
                    ssm(
                        &router,
                        case,
                        "DeleteParameter",
                        &json!({ "Name": row.name }),
                    )
                    .await;
                    expected.push(Expected {
                        operation: DELETE,
                        name: row.name.to_string(),
                        type_: row.type_.to_string(),
                        description: description.clone(),
                        arn: arn.clone(),
                    });
                }
            }
        }

        let seen = delivered_at_least(&router, case, &queue, expected.len()).await;
        assert_eq!(
            seen.len(),
            expected.len(),
            "case {case:?}: expected one envelope per write and nothing else, got {seen:?}"
        );
        for (envelope, owed) in seen.iter().zip(expected.iter()) {
            assert_envelope(case, envelope, owed);
        }
    }
}

/// A batch delete owes one envelope per parameter it really removed, and none for a name it
/// reported as invalid.
#[tokio::test]
async fn a_batch_delete_publishes_one_change_per_removed_parameter() {
    let case = "two held names and one the Region holds no parameter for";
    let router = build_router();
    let queue = given_rule_queue(
        &router,
        case,
        "parameter-events-batch",
        &json!({ "source": [EVENT_SOURCE] }),
    )
    .await;

    // The two parameters the batch removes, and the creates that owe an envelope of their
    // own: the creates are drained before the delete, so what is left is the delete's.
    let held = [
        ("/parameter-events/batch/first", "String", "first"),
        ("/parameter-events/batch/second", "StringList", "a,b"),
    ];
    let mut arns = Vec::new();
    for (name, type_, value) in held {
        ssm(
            &router,
            case,
            "PutParameter",
            &json!({ "Name": name, "Value": value, "Type": type_ }),
        )
        .await;
        let read = ssm(&router, case, "GetParameter", &json!({ "Name": name })).await;
        arns.push(
            read["Parameter"]["ARN"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected an ARN in {read}"))
                .to_string(),
        );
    }
    let creates = delivered_at_least(&router, case, &queue, held.len()).await;
    assert_eq!(
        creates.len(),
        held.len(),
        "case {case:?}: expected one envelope per create, got {creates:?}"
    );

    let unknown = "/parameter-events/batch/absent";
    let answered = ssm(
        &router,
        case,
        "DeleteParameters",
        &json!({ "Names": [held[0].0, held[1].0, unknown] }),
    )
    .await;
    assert_eq!(
        answered["DeletedParameters"],
        json!([held[0].0, held[1].0]),
        "case {case:?}: expected the two held names reported deleted, got {answered}"
    );
    assert_eq!(
        answered["InvalidParameters"],
        json!([unknown]),
        "case {case:?}: expected the unheld name reported invalid, got {answered}"
    );

    let seen = delivered_at_least(&router, case, &queue, held.len()).await;
    assert_eq!(
        seen.len(),
        held.len(),
        "case {case:?}: expected one envelope per REMOVED parameter and none for the name \
         that named nothing, got {seen:?}"
    );
    for ((envelope, (name, type_, _)), arn) in seen.iter().zip(held.iter()).zip(arns.iter()) {
        assert_envelope(
            case,
            envelope,
            &Expected {
                operation: DELETE,
                name: (*name).to_string(),
                type_: (*type_).to_string(),
                description: None,
                arn: arn.clone(),
            },
        );
    }
}

/// A call that changed no parameter publishes nothing — on a bus a write's own change
/// reaches.
///
/// The negative half is only worth anything beside that positive one: a bus that published
/// nothing ever would satisfy "no envelope" vacuously, so the refusals and the reads are made
/// BETWEEN two writes whose envelopes ARE delivered to the same rule and the same target —
/// anything they published would be queued between them.
#[tokio::test]
async fn a_refused_write_and_a_read_publish_nothing() {
    let case = "refusals and reads between two writes that are delivered";
    let router = build_router();
    let queue = given_rule_queue(
        &router,
        case,
        "parameter-events-silent",
        &json!({ "source": [EVENT_SOURCE] }),
    )
    .await;
    let name = "/parameter-events/silent";

    ssm(
        &router,
        case,
        "PutParameter",
        &json!({ "Name": name, "Value": "first", "Type": "String" }),
    )
    .await;
    let read = ssm(&router, case, "GetParameter", &json!({ "Name": name })).await;
    let arn = read["Parameter"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an ARN in {read}"))
        .to_string();

    // Each of these changes no parameter: a put of an existing name without `Overwrite`, a
    // put that would change the type, the delete of a name this Region holds no parameter
    // for, and the reads.
    let changes_nothing = [
        (
            "a put of an existing name without Overwrite",
            "PutParameter",
            json!({ "Name": name, "Value": "second", "Type": "String" }),
        ),
        (
            "a put that would change the type",
            "PutParameter",
            json!({
                "Name": name,
                "Value": "second",
                "Type": "SecureString",
                "Overwrite": true,
            }),
        ),
        (
            "a delete of a name the Region holds no parameter for",
            "DeleteParameter",
            json!({ "Name": "/parameter-events/never-written" }),
        ),
    ];
    for (refused, operation, body) in &changes_nothing {
        let (status, answered) = ssm_answered(&router, operation, body).await;
        assert_ne!(
            status, 200,
            "case {case:?}: {refused} is refused, it answered {status} with {answered}"
        );
    }
    let reads = [
        ("GetParameter", json!({ "Name": name })),
        ("GetParameters", json!({ "Names": [name] })),
        ("DescribeParameters", json!({})),
        (
            "GetParameterHistory",
            json!({ "Name": name }),
        ),
    ];
    for (operation, body) in &reads {
        ssm(&router, case, operation, body).await;
    }

    // The second write, whose envelope IS delivered: everything above happened while the
    // path was live, so the queue holds exactly the two writes' envelopes.
    ssm(
        &router,
        case,
        "PutParameter",
        &json!({ "Name": name, "Value": "second", "Type": "String", "Overwrite": true }),
    )
    .await;
    let owed = [
        Expected {
            operation: CREATE,
            name: name.to_string(),
            type_: "String".to_string(),
            description: None,
            arn: arn.clone(),
        },
        Expected {
            operation: UPDATE,
            name: name.to_string(),
            type_: "String".to_string(),
            description: None,
            arn,
        },
    ];
    let seen = delivered_at_least(&router, case, &queue, owed.len()).await;
    assert_eq!(
        seen.len(),
        owed.len(),
        "case {case:?}: expected the two writes' envelopes and nothing else, got {seen:?}"
    );
    for (envelope, owed) in seen.iter().zip(owed.iter()) {
        assert_envelope(case, envelope, owed);
    }
}

/// Which installed `EventPattern` a parameter change is owed: the envelope is routed by its
/// CONTENT, so every member the detail states is one a rule can filter on.
#[tokio::test]
async fn a_change_reaches_the_rules_whose_pattern_matches_it() {
    let case = "five patterns over one create and one update";
    let router = build_router();
    let name = "/parameter-events/routed";
    let other = "RoutedFlat";

    // (the rule, its pattern, the operations of `name` it is owed)
    let patterns: [(&str, Value, &[&str]); 5] = [
        (
            "source-only",
            json!({ "source": [EVENT_SOURCE] }),
            &[CREATE, UPDATE],
        ),
        (
            "source-and-detail-type",
            json!({ "source": [EVENT_SOURCE], "detail-type": [PARAMETER_STORE_CHANGE] }),
            &[CREATE, UPDATE],
        ),
        (
            "one-operation",
            json!({
                "source": [EVENT_SOURCE],
                "detail-type": [PARAMETER_STORE_CHANGE],
                "detail": { "operation": [CREATE] },
            }),
            &[CREATE],
        ),
        (
            "one-name-prefix",
            json!({
                "source": [EVENT_SOURCE],
                "detail": { "name": [{ "prefix": "/parameter-events/" }] },
            }),
            &[CREATE, UPDATE],
        ),
        (
            "another-source",
            json!({ "source": ["aws.example"] }),
            &[],
        ),
    ];
    let mut queues = Vec::new();
    for (rule, pattern, owed) in &patterns {
        let url = given_rule_queue(&router, case, &format!("parameter-events-{rule}"), pattern)
            .await;
        queues.push((*rule, url, *owed));
    }

    // The two writes of the routed parameter, and one write of a parameter OUTSIDE the
    // prefix the fourth pattern names: the prefix rule is owed neither of its envelopes,
    // which is what tells a filtered envelope from a withheld one.
    ssm(
        &router,
        case,
        "PutParameter",
        &json!({ "Name": name, "Value": "first", "Type": "String" }),
    )
    .await;
    let read = ssm(&router, case, "GetParameter", &json!({ "Name": name })).await;
    let arn = read["Parameter"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an ARN in {read}"))
        .to_string();
    ssm(
        &router,
        case,
        "PutParameter",
        &json!({ "Name": name, "Value": "second", "Type": "String", "Overwrite": true }),
    )
    .await;
    ssm(
        &router,
        case,
        "PutParameter",
        &json!({ "Name": other, "Value": "elsewhere", "Type": "String" }),
    )
    .await;

    for (rule, url, owed) in &queues {
        let case = &format!("{case}: the rule {rule:?}");
        // The source-only and the detail-type patterns are owed the third write's envelope
        // as well, so each queue is read for the envelopes of `name` it is owed and then
        // held to exactly the operations the pattern selects for that parameter.
        let wanted = if owed.is_empty() { 0 } else { owed.len() };
        let seen = if wanted == 0 {
            drain(&router, case, url).await
        } else {
            delivered_at_least(&router, case, url, wanted).await
        };
        let of_name: Vec<&str> = seen
            .iter()
            .filter(|envelope| envelope["detail"]["name"] == json!(name))
            .map(|envelope| {
                envelope["detail"]["operation"]
                    .as_str()
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(
            of_name, *owed,
            "case {case:?}: expected the operations {owed:?} of {name:?}, got {seen:?}"
        );
        for envelope in seen
            .iter()
            .filter(|envelope| envelope["detail"]["name"] == json!(name))
        {
            let operation = envelope["detail"]["operation"].as_str().unwrap_or_default();
            assert_envelope(
                case,
                envelope,
                &Expected {
                    operation: if operation == CREATE { CREATE } else { UPDATE },
                    name: name.to_string(),
                    type_: "String".to_string(),
                    description: None,
                    arn: arn.clone(),
                },
            );
        }
    }
}
