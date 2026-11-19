// SPDX-License-Identifier: CC0-1.0

//! What a stack still OWNS after a create it was asked NOT to roll back, and what an update
//! that re-keys a resource creates — in the service that owns those resources.
//!
//! Two rules over one estate: CloudFormation decides, Amazon SQS holds the queue. Both halves
//! need the two crates to agree, so the suite lives here — a CloudFormation-only test can read
//! the stack's own resource list but not whether the queue behind an entry of it still answers.
//!
//! **Rule 1 — a failed create whose failure the request asked to KEEP keeps every resource it
//! had already made.** `CreateStack`'s `OnFailure` takes `DO_NOTHING | ROLLBACK | DELETE`, and
//! `DO_NOTHING` ("if the stack creation fails, do nothing", which `DisableRollback` also
//! spells) settles the stack in `CREATE_FAILED`: each resource that reached `CREATE_COMPLETE`
//! before the failure is left exactly where it is — still enumerated by
//! `DescribeStackResources` under the physical id it was created with, and still usable in the
//! service that owns it. Under the documented default the same create is rolled back and those
//! resources are gone, which is what makes the two outcomes distinguishable rather than a
//! matter of wording.
//!
//! **Rule 2 — a deployment that changes a CREATE-ONLY property of a resource the stack has
//! REPLACES that resource.** CloudFormation creates the new physical resource, re-points the
//! logical id at it and removes the one it replaced, so once the stack is `UPDATE_COMPLETE`
//! the new identity resolves in the owning service — it is what the stack publishes for the
//! resource, through `Ref` and through the `PhysicalResourceId` its resource list reports —
//! and the identity it replaced resolves to nothing. For `AWS::SQS::Queue` the create-only
//! properties are the ones only `CreateQueue` takes — its name, and the attribute that
//! designates it FIFO — so a re-keyed queue is a replaced queue.
//!
//! The rows of each half differ along what its rule generalises over. Rule 1: which member
//! spells the failure action (`OnFailure=DO_NOTHING`, `DisableRollback=true`, the default,
//! `OnFailure=ROLLBACK`), whether the failure is kept or rolled back, and how many resources
//! the create had finished before it. Rule 2: whether the rename is the only change or arrives
//! with a mutable attribute change, whether the queue is standard or FIFO, and whether the
//! stack holds a second queue the same deployment does NOT re-key — which keeps its identity
//! while its neighbour is replaced.
//!
//! Every wait here is on the CONDITION the test needs (a stack status that is no longer in
//! flight), looked at again until it holds or [`SETTLE_DEADLINE`] passes; nothing asserts on
//! elapsed time.
//!
//! The awsQuery form encoder and the XML element readers are the test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of, pairs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region the stacks and their queues
/// live in.
const REGION: &str = "us-east-1";

/// The account id every queue url this emulator mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// A ceiling on a WAIT and nothing else: many times this emulator's own stack-transition span,
/// so a loaded host still gets there and a stack that never settles fails as a failure rather
/// than as a hang.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long one look at a stack's status waits before the next.
const POLL: Duration = Duration::from_millis(20);

/// One past the published upper bound of a queue's visibility timeout ("An integer from 0 to
/// 43,200 (12 hours)"), so the queue declared with it is one `CreateQueue` REFUSES — which is
/// how a row of rule 1 fails one resource of a stack without failing the rest.
const REFUSED_VISIBILITY_TIMEOUT: u32 = 43_201;

/// The status a stack rests in when a create failed and the request asked for that state to be
/// kept.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The status a stack rests in when the same create was rolled back instead.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The status an update that carried its change out rests in.
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The resource-level status of a resource an operation finished creating.
const CREATE_COMPLETE_RESOURCE: &str = "CREATE_COMPLETE";

/// The error code SQS answers for a queue name nothing holds.
const QUEUE_DOES_NOT_EXIST: &str = "QueueDoesNotExist";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
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
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
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
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call that must be accepted, answered with its XML body.
async fn cloudformation_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = cloudformation(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Call one Amazon SQS operation over its awsJson1_0 wire.
async fn sqs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One SQS call that must succeed.
async fn sqs_ok(router: &MultiServiceRouter, case: &str, operation: &str, request: Value) -> Value {
    let (status, parsed) = sqs(router, operation, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// The wire error code of an SQS refusal: awsJson1_0 carries it as the body's `__type`.
fn sqs_error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The url a queue named `name` is addressed at in this emulator.
fn queue_url(name: &str) -> String {
    format!("https://sqs.{REGION}.amazonaws.com/{ACCOUNT_ID}/{name}")
}

/// The url SQS resolves `name` to, or `None` where it holds no queue of that name — a refusal
/// that must be the modelled one, so a 500 or another code is reported rather than read as
/// "no such queue".
async fn resolved_queue(router: &MultiServiceRouter, case: &str, name: &str) -> Option<String> {
    let (status, answer) = sqs(router, "GetQueueUrl", &json!({"QueueName": name})).await;
    if status == 200 {
        return Some(
            answer
                .get("QueueUrl")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        );
    }
    assert_eq!(
        status, 400,
        "case {case:?}: expected SQS to resolve {name:?} or refuse with HTTP 400, got {status} \
         with {answer}"
    );
    let code = sqs_error_code(&answer);
    assert_eq!(
        code, QUEUE_DOES_NOT_EXIST,
        "case {case:?}: expected {QUEUE_DOES_NOT_EXIST} for {name:?}, got {code} with {answer}"
    );
    None
}

/// Every attribute SQS reports for the queue at `url`.
async fn reported_attributes(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
) -> BTreeMap<String, String> {
    let reported = sqs_ok(
        router,
        case,
        "GetQueueAttributes",
        json!({"QueueUrl": url, "AttributeNames": ["All"]}),
    )
    .await;
    reported
        .get("Attributes")
        .and_then(Value::as_object)
        .map(|attributes| {
            attributes
                .iter()
                .filter_map(|(name, value)| {
                    value.as_str().map(|value| (name.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Send one message to the queue at `url` and read it back, so the queue is shown to be USABLE
/// rather than merely resolvable. `fifo` says whether the queue needs the members a FIFO queue
/// requires of a send.
async fn round_trips_a_message(router: &MultiServiceRouter, case: &str, url: &str, fifo: bool) {
    let body = format!("{case} message");
    let mut request = json!({"QueueUrl": url, "MessageBody": body});
    if fifo {
        // "The message group ID is required for FIFO queues", and a deduplication id is
        // required of a FIFO queue whose create did not enable content-based deduplication.
        // Deduplication is per QUEUE and every queue here is a fresh one, so one fixed token
        // is enough — and it carries only the alphanumeric characters that member takes.
        request["MessageGroupId"] = json!("group");
        request["MessageDeduplicationId"] = json!("first");
    }
    sqs_ok(router, case, "SendMessage", request).await;
    let received = sqs_ok(
        router,
        case,
        "ReceiveMessage",
        json!({"QueueUrl": url, "MaxNumberOfMessages": 1}),
    )
    .await;
    let bodies: Vec<&str> = received
        .get("Messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter_map(|message| message.get("Body").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        bodies,
        vec![body.as_str()],
        "case {case:?}: expected the queue at {url} to hand back the message sent to it, got \
         {received}"
    );
}

/// The stack as `DescribeStacks` reports it once the operation in flight on it has SETTLED:
/// looked at again until its status is no longer an in-flight one, or [`SETTLE_DEADLINE`]
/// passes.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = cloudformation_ok(
            router,
            case,
            "DescribeStacks",
            &[("StackName", stack_name)],
        )
        .await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") || Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The status `DescribeStacks` reports for the stack in `body`.
fn stack_status(body: &str) -> String {
    opt_element(body, "StackStatus").unwrap_or_default()
}

/// The stack's resources as `DescribeStackResources` reports them: the logical id of each
/// against its physical id and its resource-level status, each pair read from the member that
/// carries both.
async fn stack_resources(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let body = cloudformation_ok(
        router,
        case,
        "DescribeStackResources",
        &[("StackName", stack_name)],
    )
    .await;
    (
        physical_ids_of(&body),
        pairs_of(&body, "LogicalResourceId", "ResourceStatus"),
    )
}

/// The logical id of the resource whose create is REFUSED in every row of rule 1.
const BROKEN_LOGICAL_ID: &str = "Broken";

/// The logical id of the n-th resource a rule 1 row creates before that failure.
fn good_logical_id(index: usize) -> String {
    format!("Good{index}")
}

/// A template whose queues `created` names are declared first, and whose last resource is a
/// queue `CreateQueue` refuses — held behind the others with `DependsOn`, so the refusal
/// happens only once every one of them has been created.
fn partially_failing_template(created: &[&str], broken_name: &str) -> String {
    let mut resources = String::new();
    let mut dependencies = Vec::new();
    for (index, name) in created.iter().enumerate() {
        let logical_id = good_logical_id(index);
        resources.push_str(&format!(
            r#""{logical_id}":{{"Type":"AWS::SQS::Queue","Properties":{{"QueueName":"{name}"}}}},"#
        ));
        dependencies.push(format!("\"{logical_id}\""));
    }
    resources.push_str(&format!(
        r#""{BROKEN_LOGICAL_ID}":{{"Type":"AWS::SQS::Queue","DependsOn":[{depends}],
             "Properties":{{"QueueName":"{broken_name}",
               "VisibilityTimeout":{REFUSED_VISIBILITY_TIMEOUT}}}}}"#,
        depends = dependencies.join(",")
    ));
    format!(r#"{{"Resources":{{{resources}}}}}"#)
}

/// One row of rule 1: how the create spells what to do about a failure, how many queues it
/// finishes before failing, and what must be left of them.
struct KeptRow {
    case: &'static str,
    stack_name: &'static str,
    /// The members of the `CreateStack` request that spell the failure action, which is the
    /// only thing a row varies about the call.
    failure_action: &'static [(&'static str, &'static str)],
    /// The queues the create finishes before the refusal, in declaration order.
    created: &'static [&'static str],
    /// The status the stack must rest in.
    expected_status: &'static str,
    /// Whether the resources already created must still be there.
    kept: bool,
}

/// The rows differ along what the rule generalises over: which member spells the failure action
/// — the two spellings of keeping the failed state, the documented default, and that default
/// spelled out — and how many resources the create had finished before it failed. The rolled
/// back rows are what make the kept ones mean something: the same template, the same refusal,
/// and the opposite outcome for the resources.
const KEPT_ROWS: &[KeptRow] = &[
    KeptRow {
        case: "OnFailure=DO_NOTHING, one queue created before the failure",
        stack_name: "kept-do-nothing-stack",
        failure_action: &[("OnFailure", "DO_NOTHING")],
        created: &["kept-do-nothing-queue"],
        expected_status: CREATE_FAILED,
        kept: true,
    },
    KeptRow {
        case: "DisableRollback=true spells the same thing",
        stack_name: "kept-disable-rollback-stack",
        failure_action: &[("DisableRollback", "true")],
        created: &["kept-disable-rollback-queue"],
        expected_status: CREATE_FAILED,
        kept: true,
    },
    KeptRow {
        case: "OnFailure=DO_NOTHING, three queues created before the failure",
        stack_name: "kept-three-stack",
        failure_action: &[("OnFailure", "DO_NOTHING")],
        created: &["kept-three-first", "kept-three-second", "kept-three-third"],
        expected_status: CREATE_FAILED,
        kept: true,
    },
    KeptRow {
        case: "no failure action, the documented default",
        stack_name: "rolled-back-default-stack",
        failure_action: &[],
        created: &["rolled-back-default-queue"],
        expected_status: ROLLBACK_COMPLETE,
        kept: false,
    },
    KeptRow {
        case: "OnFailure=ROLLBACK, that default spelled out",
        stack_name: "rolled-back-explicit-stack",
        failure_action: &[("OnFailure", "ROLLBACK")],
        created: &["rolled-back-explicit-first", "rolled-back-explicit-second"],
        expected_status: ROLLBACK_COMPLETE,
        kept: false,
    },
];

#[tokio::test]
async fn a_failed_create_keeps_what_it_made_only_where_the_request_asked_for_it() {
    let router = build_router();

    for row in KEPT_ROWS {
        let case = row.case;
        let broken_name = format!("{}-broken", row.stack_name);
        let template = partially_failing_template(row.created, &broken_name);
        let mut members = vec![
            ("StackName", row.stack_name),
            ("TemplateBody", template.as_str()),
        ];
        members.extend(row.failure_action.iter().copied());
        cloudformation_ok(&router, case, "CreateStack", &members).await;

        let described = settled_stack(&router, case, row.stack_name).await;
        let status = stack_status(&described);
        assert_eq!(
            status, row.expected_status,
            "case {case:?}: expected the stack to rest in {}, got {status} with {described}",
            row.expected_status
        );

        // The resource whose create was refused exists nowhere, whatever the failure action:
        // the one thing a refused create tells you is that the resource was not made.
        let broken = resolved_queue(&router, case, &broken_name).await;
        assert_eq!(
            broken, None,
            "case {case:?}: expected no queue named {broken_name:?}, got {broken:?}"
        );

        let (physical_ids, statuses) = stack_resources(&router, case, row.stack_name).await;
        for (index, name) in row.created.iter().enumerate() {
            let logical_id = good_logical_id(index);
            let resolved = resolved_queue(&router, case, name).await;
            if !row.kept {
                assert_eq!(
                    resolved, None,
                    "case {case:?}: expected the rolled back create to leave no queue named \
                     {name:?}, got {resolved:?}"
                );
                continue;
            }
            assert_eq!(
                resolved.as_deref(),
                Some(queue_url(name).as_str()),
                "case {case:?}: expected the queue created before the failure to survive at \
                 {}, got {resolved:?}",
                queue_url(name)
            );
            assert_eq!(
                physical_ids.get(&logical_id).map(String::as_str),
                Some(queue_url(name).as_str()),
                "case {case:?}: expected {logical_id} to still be enumerated under {}, got the \
                 resources {physical_ids:?}",
                queue_url(name)
            );
            assert_eq!(
                statuses.get(&logical_id).map(String::as_str),
                Some(CREATE_COMPLETE_RESOURCE),
                "case {case:?}: expected {logical_id} to still report \
                 {CREATE_COMPLETE_RESOURCE}, got the statuses {statuses:?}"
            );
            // ...and it is a working queue, not just a name that resolves: DO_NOTHING exists so
            // the partial deployment can be inspected and used.
            round_trips_a_message(&router, case, &queue_url(name), false).await;
        }
    }
}

/// The logical id every rule 2 row declares its re-keyed queue under.
const REPLACED_LOGICAL_ID: &str = "Queue";

/// The output key every rule 2 row publishes that queue's own handle under (`Ref` on an
/// `AWS::SQS::Queue` returns its url).
const REPLACED_OUTPUT: &str = "Url";

/// A template declaring one `AWS::SQS::Queue` carrying `properties` and publishing its url.
fn one_queue_template(properties: &str) -> String {
    format!(
        r#"{{"Resources":{{"{REPLACED_LOGICAL_ID}":{{"Type":"AWS::SQS::Queue",
             "Properties":{properties}}}}},
           "Outputs":{{"{REPLACED_OUTPUT}":{{"Value":{{"Ref":"{REPLACED_LOGICAL_ID}"}}}}}}}}"#
    )
}

/// One row of rule 2: the properties the queue is created with, the properties the update
/// re-keys it with, and what the replacement must be.
struct ReplacementRow {
    case: &'static str,
    stack_name: &'static str,
    /// The `Properties` object the create declares, as template text.
    created: &'static str,
    /// The `Properties` object the update declares, as template text.
    updated: &'static str,
    /// The name the create gave the queue, which the update takes away.
    old_name: &'static str,
    /// The name the update asks for, under which the replacement must exist.
    new_name: &'static str,
    /// Attributes `GetQueueAttributes` must report for the replacement.
    expected_attributes: &'static [(&'static str, &'static str)],
    /// Whether the replacement is a FIFO queue, which a send has to address as one.
    fifo: bool,
}

/// The rows differ along what the rule generalises over: whether the create-only change is the
/// only change in the deployment or arrives together with a mutable one, whether the queue is
/// standard or FIFO, and whether the declaration states other properties the replacement has to
/// be created with.
const REPLACEMENT_ROWS: &[ReplacementRow] = &[
    ReplacementRow {
        case: "a rename and nothing else",
        stack_name: "replace-plain-stack",
        created: r#"{"QueueName":"replace-plain-first"}"#,
        updated: r#"{"QueueName":"replace-plain-second"}"#,
        old_name: "replace-plain-first",
        new_name: "replace-plain-second",
        // "An integer from 0 to 43,200 (12 hours). Default: 30": the replacement is created
        // with the default, since the declaration states none.
        expected_attributes: &[("VisibilityTimeout", "30")],
        fifo: false,
    },
    ReplacementRow {
        case: "a rename carrying a mutable change with it",
        stack_name: "replace-both-stack",
        created: r#"{"QueueName":"replace-both-first","VisibilityTimeout":60}"#,
        updated: r#"{"QueueName":"replace-both-second","VisibilityTimeout":120,
             "MessageRetentionPeriod":1209600}"#,
        old_name: "replace-both-first",
        new_name: "replace-both-second",
        expected_attributes: &[
            ("VisibilityTimeout", "120"),
            ("MessageRetentionPeriod", "1209600"),
        ],
        fifo: false,
    },
    ReplacementRow {
        case: "a renamed FIFO queue",
        stack_name: "replace-fifo-stack",
        created: r#"{"QueueName":"replace-fifo-first.fifo","FifoQueue":true,
             "MessageRetentionPeriod":600}"#,
        updated: r#"{"QueueName":"replace-fifo-second.fifo","FifoQueue":true,
             "MessageRetentionPeriod":600}"#,
        old_name: "replace-fifo-first.fifo",
        new_name: "replace-fifo-second.fifo",
        expected_attributes: &[("FifoQueue", "true"), ("MessageRetentionPeriod", "600")],
        fifo: true,
    },
];

#[tokio::test]
async fn an_update_that_re_keys_a_queue_creates_the_replacement_and_drops_the_one_it_replaced() {
    let router = build_router();

    for row in REPLACEMENT_ROWS {
        let case = row.case;
        let created = one_queue_template(row.created);
        cloudformation_ok(
            &router,
            case,
            "CreateStack",
            &[
                ("StackName", row.stack_name),
                ("TemplateBody", created.as_str()),
            ],
        )
        .await;
        let described = settled_stack(&router, case, row.stack_name).await;
        let status = stack_status(&described);
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete before the rename, got {status} \
             with {described}"
        );
        let before = resolved_queue(&router, case, row.old_name).await;
        assert_eq!(
            before.as_deref(),
            Some(queue_url(row.old_name).as_str()),
            "case {case:?}: expected the create to make {}, got {before:?}",
            row.old_name
        );

        let updated = one_queue_template(row.updated);
        cloudformation_ok(
            &router,
            case,
            "UpdateStack",
            &[
                ("StackName", row.stack_name),
                ("TemplateBody", updated.as_str()),
            ],
        )
        .await;
        let described = settled_stack(&router, case, row.stack_name).await;
        let status = stack_status(&described);
        assert_eq!(
            status, UPDATE_COMPLETE,
            "case {case:?}: expected the replacing update to complete, got {status} with \
             {described}"
        );

        // The replacement exists in the service that owns it, under the name the update asked
        // for...
        let expected_url = queue_url(row.new_name);
        let resolved = resolved_queue(&router, case, row.new_name).await;
        assert_eq!(
            resolved.as_deref(),
            Some(expected_url.as_str()),
            "case {case:?}: expected the replacement queue at {expected_url}, got {resolved:?}"
        );
        // ...it is the handle the stack publishes for the resource, through its output and
        // through its resource list...
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get(REPLACED_OUTPUT).map(String::as_str),
            Some(expected_url.as_str()),
            "case {case:?}: expected the stack to publish {expected_url}, got the outputs \
             {outputs:?}"
        );
        let (physical_ids, statuses) = stack_resources(&router, case, row.stack_name).await;
        assert_eq!(
            physical_ids.get(REPLACED_LOGICAL_ID).map(String::as_str),
            Some(expected_url.as_str()),
            "case {case:?}: expected {REPLACED_LOGICAL_ID} to report {expected_url}, got the \
             resources {physical_ids:?}"
        );
        assert_eq!(
            statuses.get(REPLACED_LOGICAL_ID).map(String::as_str),
            Some(UPDATE_COMPLETE),
            "case {case:?}: expected {REPLACED_LOGICAL_ID} to report {UPDATE_COMPLETE}, got the \
             statuses {statuses:?}"
        );
        // ...it carries the configuration the declaration states...
        let attributes = reported_attributes(&router, case, &expected_url).await;
        for (name, expected) in row.expected_attributes {
            assert_eq!(
                attributes.get(*name).map(String::as_str),
                Some(*expected),
                "case {case:?}: expected the replacement's {name} to be {expected}, got the \
                 attributes {attributes:?}"
            );
        }
        // ...it works...
        round_trips_a_message(&router, case, &expected_url, row.fifo).await;
        // ...and the resource it replaced is gone.
        let old = resolved_queue(&router, case, row.old_name).await;
        assert_eq!(
            old, None,
            "case {case:?}: expected the replaced queue {} to be removed, got {old:?}",
            row.old_name
        );
    }
}

#[tokio::test]
async fn a_replacement_leaves_the_resources_the_same_deployment_did_not_re_key_alone() {
    let router = build_router();
    let case = "one of two queues re-keyed";
    let stack_name = "replace-one-of-two-stack";
    let kept_name = "replace-one-of-two-kept";
    let template = |re_keyed: &str| {
        format!(
            r#"{{"Resources":{{
                 "Kept":{{"Type":"AWS::SQS::Queue","Properties":{{"QueueName":"{kept_name}"}}}},
                 "ReKeyed":{{"Type":"AWS::SQS::Queue","Properties":{{"QueueName":"{re_keyed}"}}}}}},
               "Outputs":{{"KeptUrl":{{"Value":{{"Ref":"Kept"}}}},
                 "ReKeyedUrl":{{"Value":{{"Ref":"ReKeyed"}}}}}}}}"#
        )
    };
    let first = "replace-one-of-two-first";
    let second = "replace-one-of-two-second";

    let created = template(first);
    cloudformation_ok(
        &router,
        case,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", created.as_str())],
    )
    .await;
    let described = settled_stack(&router, case, stack_name).await;
    let status = stack_status(&described);
    assert_eq!(
        status, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {status} with {described}"
    );
    // The queue that is NOT re-keyed is identified by what the create made it: its creation
    // instant, which a replacement could not carry over.
    let kept_created_at = reported_attributes(&router, case, &queue_url(kept_name))
        .await
        .get("CreatedTimestamp")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the queue reports no CreatedTimestamp"));

    let updated = template(second);
    cloudformation_ok(
        &router,
        case,
        "UpdateStack",
        &[("StackName", stack_name), ("TemplateBody", updated.as_str())],
    )
    .await;
    let described = settled_stack(&router, case, stack_name).await;
    let status = stack_status(&described);
    assert_eq!(
        status, UPDATE_COMPLETE,
        "case {case:?}: expected the replacing update to complete, got {status} with {described}"
    );

    let outputs = outputs_of(&described);
    assert_eq!(
        outputs.get("ReKeyedUrl").map(String::as_str),
        Some(queue_url(second).as_str()),
        "case {case:?}: expected the re-keyed queue's url to be republished, got {outputs:?}"
    );
    assert_eq!(
        outputs.get("KeptUrl").map(String::as_str),
        Some(queue_url(kept_name).as_str()),
        "case {case:?}: expected the untouched queue's url to be unchanged, got {outputs:?}"
    );
    let replacement = resolved_queue(&router, case, second).await;
    assert_eq!(
        replacement.as_deref(),
        Some(queue_url(second).as_str()),
        "case {case:?}: expected the replacement queue to exist, got {replacement:?}"
    );
    let replaced = resolved_queue(&router, case, first).await;
    assert_eq!(
        replaced, None,
        "case {case:?}: expected the replaced queue to be removed, got {replaced:?}"
    );
    let kept = resolved_queue(&router, case, kept_name).await;
    assert_eq!(
        kept.as_deref(),
        Some(queue_url(kept_name).as_str()),
        "case {case:?}: expected the untouched queue to be left alone, got {kept:?}"
    );
    let kept_now = reported_attributes(&router, case, &queue_url(kept_name)).await;
    assert_eq!(
        kept_now.get("CreatedTimestamp").map(String::as_str),
        Some(kept_created_at.as_str()),
        "case {case:?}: expected the queue the deployment did not re-key to be the one the \
         create made, got {kept_now:?}"
    );
    // ...and the stack reports BOTH resources, the re-keyed one under its new handle.
    let (physical_ids, _) = stack_resources(&router, case, stack_name).await;
    assert_eq!(
        physical_ids.get("ReKeyed").map(String::as_str),
        Some(queue_url(second).as_str()),
        "case {case:?}: expected ReKeyed to report the replacement, got {physical_ids:?}"
    );
    assert_eq!(
        physical_ids.get("Kept").map(String::as_str),
        Some(queue_url(kept_name).as_str()),
        "case {case:?}: expected Kept to report the queue it has always had, got \
         {physical_ids:?}"
    );
}

/// The name a queue url addresses: its last path segment.
fn name_of(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

#[tokio::test]
async fn a_create_only_change_other_than_the_name_replaces_the_queue_it_is_declared_for() {
    let router = build_router();
    // `FifoQueue` is the other create-only property of the type ("you can't change a standard
    // queue into a FIFO queue"), and this stack does not name its queue at all — so the rule
    // is exercised where CloudFormation owns the identity on both sides of the replacement.
    let case = "a standard queue re-declared as a FIFO queue, under a generated name";
    let stack_name = "replace-fifo-variant-stack";
    let template = |properties: &str| {
        format!(
            r#"{{"Resources":{{"Queue":{{"Type":"AWS::SQS::Queue"{properties}}}}},
               "Outputs":{{"Url":{{"Value":{{"Ref":"Queue"}}}}}}}}"#
        )
    };

    let created = template("");
    cloudformation_ok(
        &router,
        case,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", created.as_str())],
    )
    .await;
    let described = settled_stack(&router, case, stack_name).await;
    let status = stack_status(&described);
    assert_eq!(
        status, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {status} with {described}"
    );
    let standard_url = outputs_of(&described)
        .get("Url")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no queue url: {described}"));
    let standard_name = name_of(&standard_url).to_string();
    let resolved = resolved_queue(&router, case, &standard_name).await;
    assert_eq!(
        resolved.as_deref(),
        Some(standard_url.as_str()),
        "case {case:?}: expected the create to make {standard_url}, got {resolved:?}"
    );

    let updated = template(r#","Properties":{"FifoQueue":true}"#);
    cloudformation_ok(
        &router,
        case,
        "UpdateStack",
        &[("StackName", stack_name), ("TemplateBody", updated.as_str())],
    )
    .await;
    let described = settled_stack(&router, case, stack_name).await;
    let status = stack_status(&described);
    assert_eq!(
        status, UPDATE_COMPLETE,
        "case {case:?}: expected the replacing update to complete, got {status} with {described}"
    );

    let fifo_url = outputs_of(&described)
        .get("Url")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no queue url: {described}"));
    assert_ne!(
        fifo_url, standard_url,
        "case {case:?}: expected the replacement to be published under an identity of its own, \
         got {fifo_url}"
    );
    let fifo_name = name_of(&fifo_url).to_string();
    // "the name of a FIFO queue must end with the .fifo suffix", which the name CloudFormation
    // generates for the replacement therefore carries and the standard queue's did not.
    assert!(
        fifo_name.ends_with(".fifo") && !standard_name.ends_with(".fifo"),
        "case {case:?}: expected the replacement to be named as a FIFO queue and the queue it \
         replaced not to be, got {fifo_name:?} replacing {standard_name:?}"
    );
    let resolved = resolved_queue(&router, case, &fifo_name).await;
    assert_eq!(
        resolved.as_deref(),
        Some(fifo_url.as_str()),
        "case {case:?}: expected the replacement queue at {fifo_url}, got {resolved:?}"
    );
    let (physical_ids, _) = stack_resources(&router, case, stack_name).await;
    assert_eq!(
        physical_ids.get("Queue").map(String::as_str),
        Some(fifo_url.as_str()),
        "case {case:?}: expected the logical id to be re-pointed at the replacement, got \
         {physical_ids:?}"
    );
    let attributes = reported_attributes(&router, case, &fifo_url).await;
    assert_eq!(
        attributes.get("FifoQueue").map(String::as_str),
        Some("true"),
        "case {case:?}: expected the replacement to be the FIFO queue the update declared, got \
         {attributes:?}"
    );
    round_trips_a_message(&router, case, &fifo_url, true).await;
    let replaced = resolved_queue(&router, case, &standard_name).await;
    assert_eq!(
        replaced, None,
        "case {case:?}: expected the standard queue it replaced to be removed, got {replaced:?}"
    );
}
