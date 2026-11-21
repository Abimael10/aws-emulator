// SPDX-License-Identifier: CC0-1.0

//! What an event source mapping on an Amazon SQS queue does once the downstream grant its
//! function's handler needs is TAKEN OFF the execution role: Lambda, Amazon SQS, Amazon
//! DynamoDB, CloudWatch Logs, IAM and the token service on ONE engine, with a real deployment
//! package whose handler makes the downstream call itself.
//!
//! RULE: a call a handler makes with the credential of its function's execution-role session
//! is decided by the policies in force on that role AT THE INSTANT OF THE CALL — "the
//! execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`) — so once the grant for that call is gone, the addressed
//! service refuses it as IT words that error class, naming the session that made it; the
//! handler's invocation then fails, nothing is written downstream, and the message the batch
//! came from is left on the source queue, delivered as many times as that queue's own
//! `RedrivePolicy` allows and then moved to the dead-letter queue the policy names ("the
//! number of times a message is delivered to the source queue before being moved to the
//! dead-letter queue", `RedrivePolicy`). Nothing of the decision is cached: the mapping keeps
//! polling and keeps invoking, and every attempt is refused again while the grant is away.
//!
//! The rows differ along what the rule generalises over: HOW the grant leaves the role (the
//! inline policy carrying it deleted, that policy rewritten to another action of the same
//! service, and an explicit `Deny` of the call written beside the grant), WHICH reason the
//! refusal then rests on (IAM's implicit deny, and the explicit deny — two different
//! sentences, each fixed text a client compares), and HOW MANY deliveries the source queue's
//! `RedrivePolicy` allows before the move (one, and two, so a row is dead-lettered on its
//! first refused attempt and a row only after a second).
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter`: the role
//! is written through IAM's own `CreateRole`/`PutRolePolicy`/`DeleteRolePolicy`, the queues
//! through Amazon SQS's own API, the table through DynamoDB's, and the refusal is read where a
//! client reads it — the handler's own line in the function's CloudWatch Logs group, and the
//! message on the dead-letter queue. A row whose downstream write happened anyway, or whose
//! message never reached the dead-letter queue, fails here.
//!
//! The SECOND suite pins the other fence over the same chain, at the other end of it: the
//! reads a mapping's poller makes on its source are calls on another service's resource, so
//! the RESOURCE-based policy that source carries decides them beside the role's own documents
//! — a `Deny` the source states about its READERS (every principal, or the whole account
//! consuming it) withholds the read whatever the role is granted, so the pass takes nothing
//! out of the queue, the function is never invoked, and the item that function would have
//! written downstream does not exist, while one naming the execution role alone is about the
//! calls that role itself signs and leaves those reads to the role's own documents. Its rows
//! differ along what the queue's own `Policy` states and who it names, and what is NEW in
//! them is where each is read: at the END of the chain, in the downstream table. What a
//! document in force does to the poller's own reads — the same catalog of statements over both
//! kinds of event source, and the backlog polled once a deny is cleared — belongs to
//! `tests/all/event_source_poll_resource_policy.rs`, whose writer of a queue's `Policy` and
//! whose spelling of an account this suite reaches rather than copies. WHO such a statement has
//! to name for it to bind the poller at all is read over the same chain by
//! `tests/all/event_source_queue_policy_reader_scope.rs`, which shares this suite's assembly,
//! handler and downstream read.
//!
//! Nothing waits a span of its own choosing: every state a row needs is waited for by looking
//! again until it holds ([`WATCH_BUDGET`] is only the backstop against a chain that never gets
//! there), and the second suite makes its poll passes itself and awaits them, so no assertion
//! depends on how fast the host runs.
//!
//! The suite RUNS handlers, so it takes this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::{dynamodb, KEY};
use crate::event_source_mapping_role_access::{allow, given_role, iam, logs_only, QUEUE_READS};
use crate::event_source_mappings::{
    call, call_sqs, create_queue, given_enabled_mapping, messages_held, queue_held,
    queue_message_counts, router_on, send_message, DrivenPolling, LOOK_AGAIN,
};
// The rule's neighbouring suites own these: the queue's own `Policy` is written and an account
// is named the one way (`event_source_poll_resource_policy`), a log group is read the one way
// (`sqs_event_source_log_delivery`), a table ARN is read back out of DynamoDB the one way
// (`python_runtime_bundled_sdk`), and this error class is worded in one place
// (`invoke_identity_authorization`), so nothing of that is written out a second time here.
use crate::event_source_poll_authorization::BLOCKED_PASSES;
use crate::event_source_poll_resource_policy::{account_root_of, set_queue_policy};
use crate::invoke_identity_authorization::{no_allow, not_authorized};
use crate::python_runtime_bundled_sdk::given_table_arn;
use crate::sqs_event_source_log_delivery::logged_lines;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below runs under, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long each source queue hides a delivered message for, and the `Timeout` of the
/// function it maps onto: a queue only maps onto a function it outlasts, and a span this short
/// brings a message no attempt processed back promptly.
const VISIBILITY_TIMEOUT: i64 = 10;

/// The environment variable each handler reads its downstream table out of.
const TARGET_VARIABLE: &str = "TARGET_TABLE";

/// The attribute each written item records the event source of the record it came from on, so
/// an item in the table is a consequence of the record having reached the handler.
///
/// `pub(crate)` for the reason [`driven_emulator`] is: the suite over WHO a queue's own
/// `Policy` names reads the same written item.
pub(crate) const SOURCE_ATTRIBUTE: &str = "source";

/// What the handler prints before the refusal the addressed service answered. ONE spelling of
/// it, bound into the handler's source and into what the assertions look for.
const REFUSED_MARKER: &str = "downstream refused: ";

/// The IAM action of the downstream call every handler below makes.
const PUT_ITEM_ACTION: &str = "dynamodb:PutItem";

/// Another action of the SAME service, which the rewritten document states instead of the
/// grant: a role short of the grant is then still a role holding a grant on DynamoDB, so the
/// refusal is an implicit deny about this one action rather than an empty policy set.
const OTHER_ITEM_ACTION: &str = "GetItem";

/// The name of the role's inline policy that carries the DOWNSTREAM grant — the one the
/// withdrawals below act on. The reads the mapping's poller makes and the log writes the
/// runtime makes are carried by a policy of their own, so a withdrawal of the downstream grant
/// leaves the mapping polling and the function logging.
const DOWNSTREAM_POLICY: &str = "downstream-write";

/// How long a row watches for the state the clock owes it — a pass polling, the invocation
/// running, its log reaching the group, and a message no attempt processed coming back and
/// being moved to the dead-letter queue — before it is a failure.
///
/// PATIENCE, not a weakened expectation: every assertion below is unchanged. It is taken from
/// what the EMULATOR itself allows ONE attempt — the wait for a share of the run capacity
/// ([`PERMIT_WAIT`]) and the wait for the run's own answer ([`HOST_WAIT_CEILING`]) — with room
/// for the several passes and the redelivery a row waits for, because a whole-workspace run
/// shares its cores with every other suite.
const WATCH_BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(Duration::from_secs(45));

/// Lambda, Amazon SQS, Amazon DynamoDB, IAM and the token service over `engine`, as
/// `core/router/src/lib.rs` mounts them: the queue a mapping polls, the table the handler
/// writes and the role its credentials are minted of are all the records those services
/// themselves hold.
fn router_over(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = router_on(engine);
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(engine),
    ));
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(engine),
        ),
    );
    router
}

/// Those services with CloudWatch Logs beside them, which is where the first suite reads the
/// line the handler printed.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_over(&engine);
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// The same services, with the poll passes of this engine's mappings the SUITE's own
/// ([`DrivenPolling`]).
///
/// A pass is background work, and nothing a caller can read says whether one has happened
/// yet, so a row that waited a span of its own choosing for one would be betting on how fast
/// the host runs. The second suite makes each pass itself and awaits it, and then reads what
/// that pass did — which is what lets it assert that a pass delivered NOTHING.
///
/// `pub(crate)` because the suite that reads WHO a queue's own `Policy` has to name for it to
/// decide that pass (`tests/all/event_source_queue_policy_reader_scope.rs`) drives the same
/// four services beside Lambda with the same driven passes, and shares this assembly rather
/// than keeping a second copy of it.
pub(crate) fn driven_emulator() -> (MultiServiceRouter, DrivenPolling) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_over(&engine);
    let driven = DrivenPolling::claiming(&engine, &router);
    (router, driven)
}

/// The handler every function below runs: for each record of the batch it writes ONE item
/// downstream, keyed by that record's own body, and on a refusal it prints what the addressed
/// service answered and lets the refusal end the invocation — so the line in the log group and
/// the item in the table are both consequences of the call the handler really made.
fn writing_handler() -> String {
    format!(
        r#"
import os

import boto3
from botocore.exceptions import ClientError

TABLE = os.environ["{TARGET_VARIABLE}"]
CLIENT = boto3.client("dynamodb")


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    wrote = []
    for record in records:
        if record["eventSource"] != "aws:sqs":
            raise ValueError("eventSource is " + repr(record["eventSource"]))
        item = {{
            "{KEY}": {{"S": record["body"]}},
            "{SOURCE_ATTRIBUTE}": {{"S": record["eventSourceARN"]}},
        }}
        try:
            CLIENT.put_item(TableName=TABLE, Item=item)
        except ClientError as error:
            print("{REFUSED_MARKER}" + error.response["Error"]["Message"])
            raise
        wrote.append(record["body"])
    return {{"wrote": wrote}}
"#
    )
}

/// Create the function `name`, running [`writing_handler`] as `role_arn` with
/// [`TARGET_VARIABLE`] naming `table`, and wait until it reports `Active`.
///
/// `pub(crate)` for the reason [`driven_emulator`] is: the suite over WHO a queue's own
/// `Policy` names runs this same handler against the same downstream table, and shares this
/// one writer of it.
pub(crate) async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    table: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": VISIBILITY_TIMEOUT,
            "Environment": { "Variables": { TARGET_VARIABLE: table } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(crate::synchronous_invoke_run::package(&writing_handler())),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
    crate::event_source_mappings::await_function_active(router, case, name).await;
}

/// Write the role's downstream inline policy with the statements `statements` names.
async fn put_downstream_policy(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    statements: Value,
) {
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", DOWNSTREAM_POLICY),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutRolePolicy on {role:?} to answer 200, got {status}: {written}"
    );
}

/// The item the table `table` holds under `key`, or [`Value::Null`] when it holds none.
///
/// `pub(crate)` for the reason [`driven_emulator`] is: the suite over WHO a queue's own
/// `Policy` names reads the downstream table exactly this way.
pub(crate) async fn held_item(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    key: &str,
) -> Value {
    let (status, answered) = dynamodb(
        router,
        "GetItem",
        &json!({ "TableName": table, "Key": { KEY: { "S": key } } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetItem answered {status} {answered}"
    );
    answered
        .get("Item")
        .cloned()
        .unwrap_or(Value::Null)
}

/// The log group a function logs to with no `LoggingConfig` of its own: "the default
/// `/aws/lambda/<function name>`".
fn log_group_of(function: &str) -> String {
    format!("/aws/lambda/{function}")
}

/// Every body the queue `url` addresses holds right now, read through one `ReceiveMessage`
/// that hides what it hands over for long enough that this reader does not see it twice.
async fn received_bodies(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let (status, answered) = call_sqs(
        router,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "VisibilityTimeout": 300,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage answered {status} {answered}"
    );
    answered["Messages"]
        .as_array()
        .map(|handed| {
            handed
                .iter()
                .filter_map(|message| message["Body"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The assumed-role session a handler's own SDK call speaks for: the session is named for the
/// FUNCTION it was opened for, and it is the principal the refusal names.
fn session(role: &str, function: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role}/{function}",
        engine::emulator_account_id()
    )
}

/// How the downstream grant leaves the role.
#[derive(Clone, Copy, Debug)]
enum Withdrawal {
    /// The inline policy carrying it is DELETED, so the role carries nothing about DynamoDB
    /// at all.
    PolicyDeleted,
    /// That policy is REWRITTEN to another action of the same service, so the role still holds
    /// a DynamoDB grant — just not this one.
    GrantReplaced,
    /// An explicit `Deny` of the call is written BESIDE the grant, which overrides it.
    ExplicitlyDenied,
}

impl Withdrawal {
    /// The reason clause the refusal then rests on: IAM's implicit deny for a call no document
    /// allows, and the explicit-deny clause for one a document forbids.
    fn reason(self) -> String {
        match self {
            Self::PolicyDeleted | Self::GrantReplaced => no_allow(PUT_ITEM_ACTION),
            Self::ExplicitlyDenied => {
                "with an explicit deny in an identity-based policy".to_string()
            }
        }
    }

    /// Take the grant off the role `role`, the way this row states.
    async fn apply(self, router: &MultiServiceRouter, case: &str, role: &str, table_arn: &str) {
        match self {
            Self::PolicyDeleted => {
                let (status, deleted) = iam(
                    router,
                    "DeleteRolePolicy",
                    &[("RoleName", role), ("PolicyName", DOWNSTREAM_POLICY)],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected DeleteRolePolicy on {role:?} to answer 200, got \
                     {status}: {deleted}"
                );
            }
            Self::GrantReplaced => {
                put_downstream_policy(
                    router,
                    case,
                    role,
                    json!([allow("dynamodb", &[OTHER_ITEM_ACTION], table_arn)]),
                )
                .await;
            }
            Self::ExplicitlyDenied => {
                put_downstream_policy(
                    router,
                    case,
                    role,
                    json!([
                        allow("dynamodb", &["PutItem"], table_arn),
                        json!({
                            "Effect": "Deny",
                            "Action": [PUT_ITEM_ACTION],
                            "Resource": table_arn,
                        }),
                    ]),
                )
                .await;
            }
        }
    }
}

/// One row: how the grant is withdrawn, and how many deliveries the source queue's
/// `RedrivePolicy` allows before the message is moved to the dead-letter queue.
struct Row {
    case: &'static str,
    /// The suffix every resource of this row is named with, so the rows share no queue, table,
    /// role, function or log group.
    slug: &'static str,
    withdrawal: Withdrawal,
    /// "The number of times a message is delivered to the source queue before being moved to
    /// the dead-letter queue" (`RedrivePolicy`).
    max_receive_count: i64,
}

/// RULE: once the downstream grant is off the execution role, the handler's call is refused as
/// the addressed service words that error class — naming the session the invocation runs as —
/// the invocation fails, nothing is written downstream, and the message is delivered as many
/// times as the source queue's `RedrivePolicy` allows and then moved to the dead-letter queue
/// it names.
#[tokio::test]
async fn a_revoked_downstream_grant_denies_the_handler_and_dead_letters_the_message() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let rows = [
        Row {
            case: "the policy carrying the grant deleted, one delivery allowed",
            slug: "deleted",
            withdrawal: Withdrawal::PolicyDeleted,
            max_receive_count: 1,
        },
        Row {
            case: "the grant replaced by another action of the service, two deliveries allowed",
            slug: "replaced",
            withdrawal: Withdrawal::GrantReplaced,
            max_receive_count: 2,
        },
        Row {
            case: "the call explicitly denied beside the grant, one delivery allowed",
            slug: "denied",
            withdrawal: Withdrawal::ExplicitlyDenied,
            max_receive_count: 1,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = emulator();

        let table = format!("esm-revoked-{}", row.slug);
        let table_arn = given_table_arn(&router, case, &table).await;

        let (dlq_url, dlq_arn) =
            create_queue(&router, case, &format!("esm-revoked-{}-dlq", row.slug), &json!({})).await;
        let (source_url, source_arn) = create_queue(
            &router,
            case,
            &format!("esm-revoked-{}-source", row.slug),
            &json!({
                "VisibilityTimeout": VISIBILITY_TIMEOUT.to_string(),
                "RedrivePolicy": json!({
                    "deadLetterTargetArn": dlq_arn,
                    "maxReceiveCount": row.max_receive_count,
                })
                .to_string(),
            }),
        )
        .await;

        // The role: the reads the mapping's poller makes and the log writes the runtime makes
        // in one policy, and the DOWNSTREAM grant in a policy of its own — so a withdrawal of
        // the latter leaves the mapping polling and the function logging.
        let role = format!("esm-revoked-{}-role", row.slug);
        let role_arn = given_role(
            &router,
            case,
            &role,
            Some(json!([
                allow("sqs", &QUEUE_READS, &source_arn),
                logs_only(),
            ])),
        )
        .await;
        put_downstream_policy(
            &router,
            case,
            &role,
            json!([allow("dynamodb", &["PutItem"], &table_arn)]),
        )
        .await;

        let function = format!("esm-revoked-{}-fn", row.slug);
        given_function(&router, case, &function, &role_arn, &table).await;
        given_enabled_mapping(
            &router,
            case,
            &json!({
                "FunctionName": function,
                "EventSourceArn": source_arn,
                "BatchSize": 1,
                "Enabled": true,
            }),
        )
        .await;

        // While the grant stands, the chain works end to end: the message is polled, the
        // handler's own call is served, and the item it wrote is in the table where a client
        // reads it.
        let granted = format!("granted-{}", row.slug);
        send_message(&router, case, &source_url, &granted, None).await;
        let expected = json!({
            KEY: { "S": granted.clone() },
            SOURCE_ATTRIBUTE: { "S": source_arn.clone() },
        });
        let deadline = Instant::now() + WATCH_BUDGET;
        let mut held = Value::Null;
        while Instant::now() < deadline {
            held = held_item(&router, case, &table, &granted).await;
            if held == expected {
                break;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        assert_eq!(
            held, expected,
            "case {case:?}: the granted write must reach the table, expected {expected}, got \
             {held}"
        );

        // The withdrawal, and the message that follows it.
        row.withdrawal
            .apply(&router, case, &role, &table_arn)
            .await;
        let refused = format!("refused-{}", row.slug);
        send_message(&router, case, &source_url, &refused, None).await;

        // The message is left on the queue by every refused attempt, delivered as many times as
        // the `RedrivePolicy` allows, and then moved to the dead-letter queue.
        let deadline = Instant::now() + WATCH_BUDGET;
        let mut dead_lettered = 0;
        let mut counts = (u32::MAX, u32::MAX, u32::MAX);
        while Instant::now() < deadline {
            dead_lettered = queue_held(&router, case, &dlq_url).await;
            counts = queue_message_counts(&router, case, &source_url).await;
            if dead_lettered == 1 && messages_held(counts) == 0 {
                break;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        assert_eq!(
            (dead_lettered, messages_held(counts)),
            (1, 0),
            "case {case:?}: the refused message must reach the dead-letter queue and leave the \
             source queue, got {counts:?} as (visible, in flight, delayed) on the source"
        );
        let dead_letters = received_bodies(&router, case, &dlq_url).await;
        assert_eq!(
            dead_letters,
            vec![refused.clone()],
            "case {case:?}: the dead-letter queue must hold exactly the refused message, got \
             {dead_letters:?}"
        );

        // Nothing of that message was written downstream: the call it would have taken was
        // refused, so the table holds no item for it.
        let absent = held_item(&router, case, &table, &refused).await;
        assert_eq!(
            absent,
            Value::Null,
            "case {case:?}: the refused write must have stored nothing, got {absent}"
        );

        // And the refusal is the sentence the addressed service words this error class with,
        // naming the session the invocation ran as — read where a client reads it, in the
        // function's own log group. There is ONE such line per delivery, so the count of them
        // is how many times the message was handed over: exactly what the `RedrivePolicy`
        // allows, and no more.
        let wanted = format!(
            "{REFUSED_MARKER}{}",
            not_authorized(
                &session(&role, &function),
                PUT_ITEM_ACTION,
                &table_arn,
                &row.withdrawal.reason(),
            )
        );
        let deliveries = usize::try_from(row.max_receive_count).unwrap_or(usize::MAX);
        let group = log_group_of(&function);
        let deadline = Instant::now() + WATCH_BUDGET;
        let mut lines = Vec::new();
        let mut refusals = 0;
        while Instant::now() < deadline {
            lines = logged_lines(&router, case, &group).await;
            refusals = lines
                .iter()
                .filter(|line| line.trim_end() == wanted)
                .count();
            if refusals >= deliveries {
                break;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        assert_eq!(
            refusals, deliveries,
            "case {case:?}: the message must have been handed over {deliveries} time(s) before \
             the move, each attempt refused with {wanted:?}, got {lines:?}"
        );
    }
}

/// What the SOURCE QUEUE's own `Policy` states while the message waits on it, and who the
/// statement names.
#[derive(Clone, Copy, Debug)]
enum QueuePolicy {
    /// Nothing at all: the queue carries no resource-based policy.
    None,
    /// An explicit deny of the receive the poller makes, stated about EVERY principal.
    DenyReceiveToEveryPrincipal,
    /// An explicit deny of the DELETE the poller makes, stated about every principal — one of
    /// the same reads, so the decision is not pinned to the receive alone. (A deny of every
    /// action of the service stated about every principal fences this suite's own client off
    /// the queue as well, exactly as it does on AWS, so what it does to the poller's reads is
    /// read in `tests/all/event_source_queue_policy_reader_scope.rs`.)
    DenyDeleteToEveryPrincipal,
    /// An explicit deny of that receive, naming the role's whole ACCOUNT — which denies each
    /// of that account's principals.
    DenyReceiveToAccount,
    /// An explicit deny of that receive, naming the execution ROLE — a statement about the
    /// calls that role itself signs rather than about the reads the mapping's poller makes.
    DenyReceiveToRole,
    /// An explicit deny of an action no consumer of a queue makes, naming the role.
    DenyUnrelatedActionToRole,
}

impl QueuePolicy {
    /// The document this row attaches to the queue, or `None` when it attaches none.
    fn document(self, role_arn: &str, queue_arn: &str) -> Option<Value> {
        // `QUEUE_READS[0]` and `[1]` are the receive and the delete the poller makes, which
        // are the reads the denies below are about; the catalog those names come from is the
        // suite that owns it.
        let receive = format!("sqs:{}", QUEUE_READS[0]);
        let delete = format!("sqs:{}", QUEUE_READS[1]);
        let about = |actions: Vec<&str>, principal: Value| {
            Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": principal,
                    "Action": actions,
                    "Resource": queue_arn,
                }],
            }))
        };
        let naming =
            |actions: Vec<&str>, principal: &str| about(actions, json!({ "AWS": principal }));
        match self {
            Self::None => None,
            Self::DenyReceiveToEveryPrincipal => about(vec![receive.as_str()], json!("*")),
            Self::DenyDeleteToEveryPrincipal => about(vec![delete.as_str()], json!("*")),
            Self::DenyReceiveToAccount => {
                naming(vec![receive.as_str()], &account_root_of(role_arn))
            }
            Self::DenyReceiveToRole => naming(vec![receive.as_str()], role_arn),
            // The WRITE half of the queue's own API, which no consumer of a queue calls, so a
            // statement about it is about none of the poller's reads.
            Self::DenyUnrelatedActionToRole => naming(vec!["sqs:SendMessage"], role_arn),
        }
    }
}

/// One row of the second suite: what the queue's own policy states, and whether the mapping
/// may then consume it — which decides both what the queue is left holding and whether the
/// item the handler would have written exists.
struct PolicyRow {
    case: &'static str,
    slug: &'static str,
    policy: QueuePolicy,
    /// Whether the pass must take the message and have the function write it downstream.
    delivers: bool,
}

/// RULE: a mapping's poller reads its event source acting as the invoked function's execution
/// role, so each read is a call on another service's resource and is decided by the
/// RESOURCE-based policy that resource carries beside the role's own documents — a `Deny` the
/// queue states about its READERS (every principal, or the whole account consuming it)
/// withholds those reads whatever the role is granted, while one naming the execution role
/// alone is about the calls that role itself signs. A pass whose read is withheld takes
/// nothing out of the queue: the message stays, the function is never invoked, and the item
/// that function writes downstream does not exist.
#[tokio::test]
async fn a_queue_policy_deny_suppresses_the_delivery_and_the_downstream_write() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let rows = [
        PolicyRow {
            case: "no policy on the queue at all",
            slug: "open",
            policy: QueuePolicy::None,
            delivers: true,
        },
        PolicyRow {
            case: "the queue denies the receive to every principal",
            slug: "deny-receive",
            policy: QueuePolicy::DenyReceiveToEveryPrincipal,
            delivers: false,
        },
        PolicyRow {
            case: "the queue denies the delete the poller makes to every principal",
            slug: "deny-delete",
            policy: QueuePolicy::DenyDeleteToEveryPrincipal,
            delivers: false,
        },
        PolicyRow {
            case: "the queue denies the receive to the role's whole account",
            slug: "deny-account",
            policy: QueuePolicy::DenyReceiveToAccount,
            delivers: false,
        },
        PolicyRow {
            case: "the queue denies the receive to the execution role itself",
            slug: "deny-role",
            policy: QueuePolicy::DenyReceiveToRole,
            delivers: true,
        },
        PolicyRow {
            case: "the queue denies an action the poller never makes",
            slug: "deny-unrelated",
            policy: QueuePolicy::DenyUnrelatedActionToRole,
            delivers: true,
        },
    ];

    for row in rows {
        let case = row.case;
        // The passes are this row's own and are awaited, so what the row reads is what the
        // pass it just made did.
        let (router, mut driven) = driven_emulator();

        let table = format!("esm-fenced-{}", row.slug);
        let table_arn = given_table_arn(&router, case, &table).await;
        let (url, queue_arn) =
            create_queue(&router, case, &format!("esm-fenced-{}", row.slug), &json!({})).await;

        // The role is granted every read of the queue and the downstream write, so the only
        // thing that can withhold a read is the row's own document on the QUEUE.
        let role = format!("esm-fenced-{}-role", row.slug);
        let role_arn = given_role(
            &router,
            case,
            &role,
            Some(json!([
                allow("sqs", &QUEUE_READS, &queue_arn),
                allow("dynamodb", &["PutItem"], &table_arn),
                logs_only(),
            ])),
        )
        .await;
        let function = format!("esm-fenced-{}-fn", row.slug);
        given_function(&router, case, &function, &role_arn, &table).await;
        given_enabled_mapping(
            &router,
            case,
            &json!({
                "FunctionName": function,
                "EventSourceArn": queue_arn,
                "BatchSize": 1,
                "Enabled": true,
            }),
        )
        .await;

        // Then the row's document, and only then a message to poll: what is in force when the
        // message arrives is what the pass is subject to.
        set_queue_policy(&router, case, &url, row.policy.document(&role_arn, &queue_arn)).await;
        let body = format!("fenced-{}", row.slug);
        send_message(&router, case, &url, &body, None).await;

        let expected = json!({
            KEY: { "S": body.clone() },
            SOURCE_ATTRIBUTE: { "S": queue_arn.clone() },
        });
        if row.delivers {
            // One pass, awaited: the batch has been handed to the function and the attempt is
            // over, so the item the handler wrote is already in the table.
            driven.poll_once().await;
            let held = held_item(&router, case, &table, &body).await;
            assert_eq!(
                held, expected,
                "case {case:?}: nothing in force withholds a read the poller makes, so the \
                 handler must have written {expected}, got {held}"
            );
            continue;
        }
        // The complement: a read the queue's own document denies is one the pass does not
        // make, so the message is still the queue's own after every pass and nothing
        // downstream was ever written.
        for pass in 0..BLOCKED_PASSES {
            driven.poll_once().await;
            let counts = queue_message_counts(&router, case, &url).await;
            assert_eq!(
                counts,
                (1, 0, 0),
                "case {case:?}: the queue's own policy denies one of the reads its poller \
                 makes, so pass {pass} must leave the message visible on the queue, got \
                 {counts:?} as (visible, in flight, delayed)"
            );
            let held = held_item(&router, case, &table, &body).await;
            assert_eq!(
                held,
                Value::Null,
                "case {case:?}: no delivery was made, so after pass {pass} the downstream table \
                 must hold nothing for the message, got {held}"
            );
        }
    }
}
