// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a `CreateEventSourceMapping` onto an Amazon SQS queue answers when the queue
//! is not there AND the invoked function's execution role is short of the reads a poller of it
//! makes — two conditions whose fixes are opposite ones.
//!
//! RULE: an event source whose existence the service that owns it settles is RESOLVED IN THAT
//! SERVICE FIRST, and a create that finds nothing there is refused with the failure that
//! service answers the read with, reported as the read Lambda attempted ("Lambda reads items
//! from the event source and invokes the function",
//! `smithy_query(query="operation", operation="CreateEventSourceMapping")`) — so an absent
//! source is never diagnosed as a verdict about a policy, whatever the role carries. Only a
//! source that DOES resolve is then judged against the grants in force on that role, and that
//! refusal is AWS's fixed sentence naming the role as the function's execution role, the call
//! it may not make and the service it would have been made to. Either refusal is the modelled
//! `InvalidParameterValueException` at HTTP 400 and stores no mapping.
//!
//! The rows differ along what the rule generalises over: whether the named queue was created
//! at all, WHAT the role is granted (nothing but the log writes, every read on the held queue
//! only, every read on every resource, every read on exactly the name nothing was created
//! under, the reads short of one of the set), WHICH read is the one withheld when the queue is
//! there, and which kind of queue the source is (standard and FIFO, whose names differ by the
//! suffix Amazon SQS requires of a FIFO queue). Every row drives real HTTP requests through
//! the production `MultiServiceRouter` with IAM and Amazon SQS mounted beside Lambda on ONE
//! engine, so the queue is the one the queue service holds and the documents are the ones
//! `CreateRole`/`PutRolePolicy` persisted, and reads the outcome back through
//! `ListEventSourceMappings` and `GetEventSourceMapping`.

use serde_json::{json, Value};

use crate::event_source_mapping_absent_queue::ABSENT_QUEUE_REFUSAL;
use crate::event_source_mapping_role_access::{
    allow, assert_refused, call_refusal, create_function_as, given_role, logs_only, QUEUE_READS,
};
use crate::event_source_mappings::{
    create_mapping, create_queue, create_request, get_mapping, list_mappings, refusal_message,
};
use crate::event_source_read_refusal_sentence::router_with_iam_and_queues;

/// The Region the harness signs for, which the queue ARNs below are written in.
const REGION: &str = "us-east-1";

/// The account id every emulator ARN carries.
const ACCOUNT: &str = "000000000000";

/// The name of the queue every row creates in the queue service — the source a row either
/// maps or writes its grant about.
const HELD_QUEUE: &str = "esm-precedence-intake";

/// A name nothing is ever created under, which is what makes the source of a row absent.
const ABSENT_QUEUE: &str = "esm-precedence-never-created";

/// The FIFO forms of the two, whose names Amazon SQS requires to end in `.fifo`.
const HELD_FIFO_QUEUE: &str = "esm-precedence-intake.fifo";
/// See [`HELD_FIFO_QUEUE`].
const ABSENT_FIFO_QUEUE: &str = "esm-precedence-never-created.fifo";

/// Every resource, the widest a grant can name — which still cannot make a queue exist.
const EVERY_RESOURCE: &str = "*";

/// How the sentence of a withheld read names the service the call would have been made to.
const SQS: &str = "SQS";

/// The phrase every refusal about an execution role's grants opens on, which the refusal of an
/// ABSENT source must not carry.
const ROLE_VERDICT_PHRASE: &str = "execution role";

/// The error code Amazon SQS answers a read of a queue it does not hold with, which the
/// refusal of a queue that IS there must not carry
/// (`service_com_amazonaws_sqs_amazonsqs::state::QUEUE_DOES_NOT_EXIST_CODE`).
const ABSENT_QUEUE_CODE: &str = "AWS.SimpleQueueService.NonExistentQueue";

/// The ARN of the queue `name` in this account and Region, as Amazon SQS writes it.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
}

/// What a row expects of the create.
enum Expected {
    /// The mapping is made, and reads back on the source the request named.
    Accepted,
    /// The refusal that propagates the queue service's own absence of the named queue.
    AbsentQueue,
    /// The refusal of a role that may not make the named call on the queue.
    Withheld(&'static str),
}

/// One row: which queue to create, which one the create names, what the role is granted, and
/// the outcome the rule gives.
struct Row {
    case: &'static str,
    /// The queue the queue service is given first, with the attributes it takes.
    given_queue: (&'static str, Value),
    /// The queue name the create's `EventSourceArn` addresses.
    named_queue: &'static str,
    /// The statements of the role's one inline policy.
    statements: Value,
    expected: Expected,
}

/// Every read a poller of a queue makes, granted on the queue `name`.
fn reads_on(name: &str) -> Value {
    allow("sqs", &QUEUE_READS, &queue_arn(name))
}

/// The catalog: see the module docs.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // The positive control: the queue is there and the role may read it.
            case: "a held queue, with every read granted on it",
            given_queue: (HELD_QUEUE, json!({})),
            named_queue: HELD_QUEUE,
            statements: json!([logs_only(), reads_on(HELD_QUEUE)]),
            expected: Expected::Accepted,
        },
        Row {
            // Both conditions at once, with the grant scoped to the queue that IS there: the
            // absence of the named one is what the caller is told.
            case: "an absent queue, with every read granted on the held queue only",
            given_queue: (HELD_QUEUE, json!({})),
            named_queue: ABSENT_QUEUE,
            statements: json!([logs_only(), reads_on(HELD_QUEUE)]),
            expected: Expected::AbsentQueue,
        },
        Row {
            // The role is short of every read as well, and the absence still decides.
            case: "an absent queue, with nothing but the log writes granted",
            given_queue: (HELD_QUEUE, json!({})),
            named_queue: ABSENT_QUEUE,
            statements: json!([logs_only()]),
            expected: Expected::AbsentQueue,
        },
        Row {
            // The widest grant there is cannot make the queue exist, so the answer is the
            // same: the refusal is about the source, not about the policy.
            case: "an absent queue, with every read granted on every resource",
            given_queue: (HELD_QUEUE, json!({})),
            named_queue: ABSENT_QUEUE,
            statements: json!([allow("sqs", &QUEUE_READS, EVERY_RESOURCE)]),
            expected: Expected::AbsentQueue,
        },
        Row {
            // A document naming the absent queue itself does not create it either.
            case: "an absent queue, with every read granted on exactly that name",
            given_queue: (HELD_QUEUE, json!({})),
            named_queue: ABSENT_QUEUE,
            statements: json!([logs_only(), reads_on(ABSENT_QUEUE)]),
            expected: Expected::AbsentQueue,
        },
        Row {
            // The source resolves, so the role is what is left to decide — and the sentence
            // names the first read its poller makes.
            case: "a held queue, with nothing but the log writes granted",
            given_queue: (HELD_QUEUE, json!({})),
            named_queue: HELD_QUEUE,
            statements: json!([logs_only()]),
            expected: Expected::Withheld(QUEUE_READS[0]),
        },
        Row {
            // Short of one read of the set: the sentence names that one.
            case: "a held queue, with the receive and the delete granted",
            given_queue: (HELD_QUEUE, json!({})),
            named_queue: HELD_QUEUE,
            statements: json!([
                logs_only(),
                allow(
                    "sqs",
                    &[QUEUE_READS[0], QUEUE_READS[1]],
                    &queue_arn(HELD_QUEUE)
                )
            ]),
            expected: Expected::Withheld(QUEUE_READS[2]),
        },
        Row {
            // The kind of queue changes neither decision: a FIFO queue that is there is
            // judged by the role.
            case: "a held fifo queue, with nothing but the log writes granted",
            given_queue: (HELD_FIFO_QUEUE, json!({ "FifoQueue": "true" })),
            named_queue: HELD_FIFO_QUEUE,
            statements: json!([logs_only()]),
            expected: Expected::Withheld(QUEUE_READS[0]),
        },
        Row {
            // And a FIFO name nothing was created under is absent, however it is granted.
            case: "an absent fifo queue, with every read granted on it",
            given_queue: (HELD_FIFO_QUEUE, json!({ "FifoQueue": "true" })),
            named_queue: ABSENT_FIFO_QUEUE,
            statements: json!([logs_only(), reads_on(ABSENT_FIFO_QUEUE)]),
            expected: Expected::AbsentQueue,
        },
    ]
}

/// RULE: see the module docs — the source is resolved in the service that owns it first, and
/// only a source that resolves is judged against the execution role's grants.
#[tokio::test]
async fn an_absent_queue_is_refused_as_the_queue_services_own_failure_and_not_as_a_role_verdict() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = router_with_iam_and_queues();

        let (name, attributes) = &row.given_queue;
        let (_url, held_arn) = create_queue(&router, case, name, attributes).await;
        assert_eq!(
            held_arn,
            queue_arn(name),
            "case {case:?}: the queue service must report the ARN this suite writes, got \
             {held_arn}"
        );

        let role = given_role(
            &router,
            case,
            &format!("esm-precedence-{index}"),
            Some(row.statements.clone()),
        )
        .await;
        let function = format!("esm-precedence-fn-{index}");
        create_function_as(&router, case, &function, &role).await;

        let source = queue_arn(row.named_queue);
        let answer = create_mapping(
            &router,
            &create_request(&function, Some(&source), &json!({})),
        )
        .await;

        match row.expected {
            Expected::Accepted => {
                assert_eq!(
                    answer.status,
                    202,
                    "case {case:?}: {source} is a queue this mapping can read, so the create \
                     must answer 202, got {} {}",
                    answer.status,
                    answer.text()
                );
                let uuid = answer.string("UUID").unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: an accepted create must answer a UUID, got {}",
                        answer.text()
                    )
                });
                let read = get_mapping(&router, &uuid).await;
                assert_eq!(
                    (read.status, read.string("EventSourceArn")),
                    (200, Some(source.clone())),
                    "case {case:?}: the created mapping must read back on {source}, got {} {}",
                    read.status,
                    read.text()
                );
                continue;
            }
            Expected::AbsentQueue => {
                assert_refused(case, &answer, ABSENT_QUEUE_REFUSAL);
                let message = refusal_message(&answer).unwrap_or_default();
                // The point of the row: the absence of the source is not reported as a
                // verdict about the role, so the sentence says nothing about one.
                assert!(
                    !message.contains(ROLE_VERDICT_PHRASE),
                    "case {case:?}: an absent queue must not be reported as a role verdict, \
                     got {message:?}"
                );
            }
            Expected::Withheld(action) => {
                let expected = call_refusal(action, SQS);
                assert_refused(case, &answer, &expected);
                let message = refusal_message(&answer).unwrap_or_default();
                // And the complement: a source that resolves is never reported as absent.
                assert!(
                    !message.contains(ABSENT_QUEUE_CODE),
                    "case {case:?}: a queue that is there must not be reported absent, got \
                     {message:?}"
                );
            }
        }

        // Either refusal is instead of the mapping, not beside it.
        let listed = list_mappings(&router, &format!("FunctionName={function}")).await;
        assert_eq!(
            (listed.status, listed.listed_uuids()),
            (200, Vec::new()),
            "case {case:?}: a refused create must leave no mapping behind, got {} {}",
            listed.status,
            listed.text()
        );
    }
}
