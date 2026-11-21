// SPDX-License-Identifier: CC0-1.0

//! WHO a statement in a source queue's own `Policy` has to name for it to decide the reads an
//! event source mapping's poller makes on that queue — read at the END of the chain, in the
//! DynamoDB table the invoked handler itself writes.
//!
//! RULE: a queue is fenced off from its CONSUMERS by its own `Policy` attribute, so an
//! explicit `Deny` about one of the reads consuming the queue takes withholds that read from a
//! mapping's poller when the queue states it about its READERS — about every principal, or
//! about the whole account consuming the queue — and a withheld read takes nothing out of the
//! queue: nothing is handed over, invoked or deleted, the mapping keeps reporting the state it
//! was in, no refusal reaches any caller, and the messages are delivered once the document in
//! force stops withholding them. A `Deny` aimed at the function's execution role ALONE is not
//! such a statement: a poller reads with the grants of that role — which is why the role's own
//! documents decide these reads
//! (`tests/all/event_source_poll_authorization.rs`) — but the read is not one of the calls
//! that role signs, which are the ones a running handler makes with its execution-role
//! session's credential
//! (`tests/all/lambda_in_function_sqs_send_follows_the_live_policies.rs` in `core/router`
//! reads a queue policy deciding one of THOSE), so delivery runs on under it.
//!
//! What only this suite reaches is that rule's effect at the far end of the chain and the
//! whole SEQUENCE a client drives through it: a message withheld while the queue denies every
//! principal, the same mapping still delivering under a deny aimed at its execution role, and
//! that withheld message released by removing the policy — each observed where a client
//! observes it (the queue's own message counts, `GetEventSourceMapping`, and the downstream
//! item the handler wrote). What a document in force does to the poller's own reads over both
//! kinds of event source is `tests/all/event_source_poll_resource_policy.rs`, whose writer of
//! a queue's `Policy` and whose spelling of an account this suite reaches rather than copies.
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter` with Amazon
//! SQS, Amazon DynamoDB, IAM and the token service mounted beside Lambda on ONE engine: the
//! queue and its policy are written through Amazon SQS's own API, the role through IAM's own,
//! the table through DynamoDB's, and the handler really RUNS and makes the downstream write
//! itself. The rows of one test share that chain — one table, one role, one function, a queue
//! and a mapping each ([`given_chain`]) — so a run costs one standing-up between them while
//! each row's own document still fences only its own queue.
//!
//! The rows differ along what the rule generalises over: WHO the statement names (every
//! principal in either spelling of that element, the whole account in either spelling of it,
//! the execution role itself, another role of the same account) and WHAT it denies (the
//! receive the poller makes, the delete it makes, every action of the queue's service
//! — which fences this suite's own client off the queue too, exactly as it does on AWS — and
//! an action no consumer of a queue makes). The two halves are two tests — the statements that
//! withhold the poller's reads ([`withholding_rows`]) and the ones that leave them alone
//! ([`delivering_rows`]) — because what each half reads of a pass is different, and a third
//! drives the whole sequence over one queue.
//!
//! Nothing here waits a span of its own choosing: the poll passes are the suite's own and are
//! awaited ([`DrivenPolling`]), so what each assertion reads is what the pass it just made
//! did.
//!
//! The suite RUNS handlers, so it takes this target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::KEY;
use crate::event_source_mapping_role_access::{allow, given_role, logs_only, QUEUE_READS};
use crate::event_source_mappings::{
    call_sqs, create_queue, get_mapping, given_enabled_mapping, queue_message_counts,
    reports_no_attempt, send_message,
};
use crate::event_source_poll_authorization::{BLOCKED_PASSES, STATE_ENABLED};
use crate::event_source_poll_resource_policy::{account_root_of, set_queue_policy};
use crate::python_runtime_bundled_sdk::given_table_arn;
// The chain this rule is read at the END of is the one the suite over a WITHDRAWN downstream
// grant already assembles: the same four services beside Lambda with the suite's own driven
// passes, the same handler writing one item per record, and the same read of that item. Shared
// from there rather than copied, so neither suite's chain can drift from the other's.
use crate::sqs_event_source_revoked_downstream_grant::{
    driven_emulator, given_function, held_item, SOURCE_ATTRIBUTE,
};

/// The receive a mapping's poller makes on a queue, as an IAM action: `QUEUE_READS[0]` is that
/// read, and the catalog it comes from is the suite that owns it.
fn receive_action() -> String {
    format!("sqs:{}", QUEUE_READS[0])
}

/// The chain every row of a table below reads through: ONE downstream table, ONE execution
/// role, ONE function, and one queue per row with its own mapping — so a run of rows costs one
/// table, one role and one cold start between them rather than one each, while each row's own
/// document still fences only its own queue.
struct Chain {
    /// The downstream table's name, which every row's item is read out of.
    table: String,
    /// The execution role's ARN, which a statement may name.
    role_arn: String,
    /// Another role of the same account, for the statement that names the wrong principal.
    other_role_arn: String,
    /// One queue per row, in the order the slugs were given: its url, its ARN, and the UUID
    /// of the mapping that polls it.
    queues: Vec<Mapped>,
}

/// One row's queue and the mapping that polls it.
struct Mapped {
    /// The queue's url, which a send addresses.
    url: String,
    /// Its ARN, which the mapping names and the row's statement is about.
    queue_arn: String,
    /// The mapping's UUID, which a reading of its state names.
    uuid: String,
}

/// Stand the chain up for the rows `slugs` names: a queue each, a role granted every read of
/// all of them and the downstream write, a table, a function running the shared writing
/// handler as that role, and an ENABLED mapping of at most one message per batch onto each
/// queue.
///
/// The role is written BEFORE the mappings because a mapping is only wired while its role may
/// read the source, and each queue hides a delivered message for its own default span, which
/// outlasts the shared function's `Timeout` — Lambda takes a queue mapping only while it does
/// — while a message no pass took is never hidden at all, so nothing below waits that span
/// out.
async fn given_chain(router: &MultiServiceRouter, case: &str, slugs: &[&str]) -> Chain {
    let mut queues = Vec::new();
    for slug in slugs {
        let (url, queue_arn) =
            create_queue(router, case, &format!("reader-scope-{slug}"), &json!({})).await;
        queues.push((url, queue_arn));
    }
    let table = format!("reader-scope-{}", slugs.first().copied().unwrap_or("none"));
    let table_arn = given_table_arn(router, case, &table).await;
    let reads: Vec<Value> = queues
        .iter()
        .map(|(_, queue_arn)| allow("sqs", &QUEUE_READS, queue_arn))
        .collect();
    let mut statements = reads;
    statements.push(allow("dynamodb", &["PutItem"], &table_arn));
    statements.push(logs_only());
    let role_arn = given_role(
        router,
        case,
        &format!("{table}-role"),
        Some(Value::Array(statements)),
    )
    .await;
    let other_role_arn = given_role(router, case, &format!("{table}-other"), None).await;
    let function = format!("{table}-fn");
    given_function(router, case, &function, &role_arn, &table).await;
    let mut mapped = Vec::new();
    for (url, queue_arn) in queues {
        let uuid = given_enabled_mapping(
            router,
            case,
            &json!({
                "FunctionName": function,
                "EventSourceArn": queue_arn,
                "BatchSize": 1,
                "Enabled": true,
            }),
        )
        .await;
        mapped.push(Mapped {
            url,
            queue_arn,
            uuid,
        });
    }
    Chain {
        table,
        role_arn,
        other_role_arn,
        queues: mapped,
    }
}

/// Who a row's one statement names, and in which spelling of the `Principal` element.
#[derive(Clone, Copy, Debug)]
enum Named {
    /// Every principal, through the whole-element `"*"`.
    EveryPrincipal,
    /// Every principal, through the `AWS` key's own `"*"`: the other spelling of it.
    EveryAwsPrincipal,
    /// The whole account consuming the queue, by its root-user ARN.
    AccountRoot,
    /// The whole account consuming the queue, by its bare account id.
    AccountId,
    /// The function's execution role itself.
    ExecutionRole,
    /// Another role of the same account.
    AnotherRole,
}

impl Named {
    /// The `Principal` element this names, for a chain whose execution role is `chain`'s.
    fn element(self, chain: &Chain) -> Value {
        match self {
            Self::EveryPrincipal => json!("*"),
            Self::EveryAwsPrincipal => json!({ "AWS": "*" }),
            Self::AccountRoot => json!({ "AWS": account_root_of(&chain.role_arn) }),
            Self::AccountId => json!({ "AWS": account_of(&chain.role_arn) }),
            Self::ExecutionRole => json!({ "AWS": chain.role_arn.clone() }),
            Self::AnotherRole => json!({ "AWS": chain.other_role_arn.clone() }),
        }
    }
}

/// The bare account id of `arn`, which is the other spelling a statement names a whole ACCOUNT
/// by beside its root-user ARN ([`account_root_of`], whose own reading of an ARN's account this
/// follows: the fifth colon-separated field, and a panic for a value carrying none).
fn account_of(arn: &str) -> String {
    arn.split(':')
        .nth(4)
        .filter(|account| !account.is_empty())
        .unwrap_or_else(|| panic!("the ARN {arn:?} carries no account"))
        .to_string()
}

/// One row: the one `Deny` the queue carries while the message waits on it, and whether the
/// mapping may then consume that message.
struct Row {
    case: &'static str,
    slug: &'static str,
    /// Who the statement names.
    named: Named,
    /// What it denies, as the queue's service names the action — `None` for the receive the
    /// poller makes, which is read from [`receive_action`].
    denied: Option<&'static str>,
    /// Whether the statement also covers the calls this SUITE makes on the queue — a deny of
    /// every action of the service stated about every principal fences the suite's own client
    /// off too, exactly as it does on AWS. Such a row sends its message before the document
    /// is in force and reads the withhold where the document leaves it readable: in the
    /// downstream table and on the mapping itself.
    fences_the_client: bool,
}

/// The document a row attaches to its queue: one `Deny` statement about `denied`, naming
/// `named`, on that queue.
fn document(row: &Row, chain: &Chain, queue_arn: &str) -> Value {
    let denied = row
        .denied
        .map_or_else(receive_action, ::std::string::ToString::to_string);
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": row.named.element(chain),
            "Action": denied,
            "Resource": queue_arn,
        }],
    })
}

/// Put each row's document in force and send the message its pass is to decide, answering the
/// body each row's message carries.
///
/// What is in force when the PASS runs is what that pass is subject to, and a document that
/// fences this suite's own client off its queue is installed after that queue's send for that
/// reason alone.
async fn given_documents(
    router: &MultiServiceRouter,
    chain: &Chain,
    rows: &[Row],
) -> Vec<String> {
    let mut bodies = Vec::new();
    for (row, mapped) in rows.iter().zip(&chain.queues) {
        let case = row.case;
        let body = format!("reader-scope-{}", row.slug);
        let document = Some(document(row, chain, &mapped.queue_arn));
        if row.fences_the_client {
            send_message(router, case, &mapped.url, &body, None).await;
            set_queue_policy(router, case, &mapped.url, document).await;
        } else {
            set_queue_policy(router, case, &mapped.url, document).await;
            send_message(router, case, &mapped.url, &body, None).await;
        }
        bodies.push(body);
    }
    bodies
}

/// The rows whose document is about the queue's READERS, which the poller is one of.
fn withholding_rows() -> Vec<Row> {
    vec![
        Row {
            case: "the queue denies the receive to every principal",
            slug: "every",
            named: Named::EveryPrincipal,
            denied: None,
            fences_the_client: false,
        },
        Row {
            case: "the queue denies the receive to every principal named under the AWS key",
            slug: "every-aws",
            named: Named::EveryAwsPrincipal,
            denied: None,
            fences_the_client: false,
        },
        Row {
            // The SECOND of the reads consuming a queue takes rather than the first, so the
            // decision is not pinned to one action of the set.
            case: "the queue denies the delete the poller makes to every principal",
            slug: "every-delete",
            named: Named::EveryPrincipal,
            denied: Some("sqs:DeleteMessage"),
            fences_the_client: false,
        },
        Row {
            case: "the queue denies every action of its service to every principal",
            slug: "every-action",
            named: Named::EveryPrincipal,
            denied: Some("sqs:*"),
            fences_the_client: true,
        },
        Row {
            case: "the queue denies the receive to the consuming account's root",
            slug: "account-root",
            named: Named::AccountRoot,
            denied: None,
            fences_the_client: false,
        },
        Row {
            case: "the queue denies the receive to the consuming account's id",
            slug: "account-id",
            named: Named::AccountId,
            denied: None,
            fences_the_client: false,
        },
    ]
}

/// The rows whose document is about SOMEBODY ELSE, or about an action no consumer makes, and
/// so withholds none of the poller's reads.
fn delivering_rows() -> Vec<Row> {
    vec![
        Row {
            case: "the queue denies the receive to the execution role itself",
            slug: "role",
            named: Named::ExecutionRole,
            denied: None,
            fences_the_client: false,
        },
        Row {
            case: "the queue denies every action of its service to the execution role itself",
            slug: "role-every-action",
            named: Named::ExecutionRole,
            denied: Some("sqs:*"),
            fences_the_client: false,
        },
        Row {
            case: "the queue denies the receive to another role of the account",
            slug: "another-role",
            named: Named::AnotherRole,
            denied: None,
            fences_the_client: false,
        },
        Row {
            // The WRITE half of the queue's own API, which no consumer of a queue calls, so a
            // statement about it is about none of the poller's reads however widely it is
            // stated. It fences this suite's own send off, so that send goes first.
            case: "the queue denies an action no consumer of a queue makes to every principal",
            slug: "unrelated",
            named: Named::EveryPrincipal,
            denied: Some("sqs:SendMessage"),
            fences_the_client: true,
        },
    ]
}

/// RULE: a `Deny` in the source queue's own `Policy` withholds the reads a mapping's poller
/// makes when the queue states it about its READERS — every principal, or the whole account
/// consuming the queue — so nothing is handed over, invoked or deleted, the item the handler
/// would have written downstream does not exist, and the mapping is still the enabled mapping
/// it was with no attempt to report.
#[tokio::test]
async fn a_deny_about_the_queues_readers_withholds_its_mappings_reads() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let setup = "a queue per statement about its readers";
    let rows = withholding_rows();
    let slugs: Vec<&str> = rows.iter().map(|row| row.slug).collect();
    let (router, mut driven) = driven_emulator();
    let chain = given_chain(&router, setup, &slugs).await;
    let bodies = given_documents(&router, &chain, &rows).await;

    // A read the queue denies its readers is one the pass does not make, so the message is
    // still the queue's own after every pass and nothing downstream was ever written.
    for pass in 0..BLOCKED_PASSES {
        driven.poll_once().await;
        for ((row, mapped), body) in rows.iter().zip(&chain.queues).zip(&bodies) {
            let case = row.case;
            if !row.fences_the_client {
                let counts = queue_message_counts(&router, case, &mapped.url).await;
                assert_eq!(
                    counts,
                    (1, 0, 0),
                    "case {case:?}: the queue denies its readers the receive, so pass {pass} \
                     must leave the message visible on the queue, got {counts:?} as (visible, \
                     in flight, delayed)"
                );
            }
            let held = held_item(&router, case, &chain.table, body).await;
            assert_eq!(
                held,
                Value::Null,
                "case {case:?}: no delivery was made, so after pass {pass} the downstream \
                 table must hold nothing for the message, got {held}"
            );
        }
    }

    // And nothing of it reaches a caller: each mapping is the enabled mapping it was, with no
    // processing attempt to report, because no batch was polled.
    for (row, mapped) in rows.iter().zip(&chain.queues) {
        let case = row.case;
        let reading = get_mapping(&router, &mapped.uuid).await;
        assert_eq!(
            reading.string("State").as_deref(),
            Some(STATE_ENABLED),
            "case {case:?}: a mapping whose source withholds its reads is still the enabled \
             mapping it was, got {}",
            reading.text()
        );
        assert!(
            reports_no_attempt(&reading),
            "case {case:?}: no batch was polled, so there is no processing attempt to report, \
             got {}",
            reading.text()
        );
    }
}

/// RULE, the other half: a `Deny` the queue states about an identity the poller is NOT — the
/// function's execution role, whose own ARN is about the calls that role signs, or another
/// principal of the account — or about an action no consumer of a queue makes, withholds none
/// of the poller's reads, so the delivery runs on and the handler writes the record downstream.
#[tokio::test]
async fn a_deny_about_another_identity_leaves_a_mappings_reads_alone() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let setup = "a queue per statement about somebody else";
    let rows = delivering_rows();
    let slugs: Vec<&str> = rows.iter().map(|row| row.slug).collect();
    let (router, mut driven) = driven_emulator();
    let chain = given_chain(&router, setup, &slugs).await;
    let bodies = given_documents(&router, &chain, &rows).await;

    // One pass per row, awaited: each batch has been handed to the function and the attempt is
    // over, so the item the handler wrote is already in the table.
    for _ in 0..rows.len() {
        driven.poll_once().await;
    }
    for ((row, mapped), body) in rows.iter().zip(&chain.queues).zip(&bodies) {
        let case = row.case;
        let held = held_item(&router, case, &chain.table, body).await;
        let expected = json!({
            KEY: { "S": body.clone() },
            SOURCE_ATTRIBUTE: { "S": mapped.queue_arn.clone() },
        });
        assert_eq!(
            held, expected,
            "case {case:?}: the queue's own document states no deny about its readers, so the \
             handler must have written {expected}, got {held}"
        );
    }
}

/// RULE, as a client drives it end to end: the document in force is read afresh on every
/// pass, so a message withheld while the queue denies its readers is STILL the queue's own —
/// delivered the moment the document stops withholding it, whether that is a deny narrowed to
/// the execution role alone or the policy removed outright ("you can remove the `Policy`
/// attribute by setting it to an empty string"). Nothing of the withhold is reported to the
/// caller while it lasts.
#[tokio::test]
async fn a_message_withheld_by_the_queues_policy_is_delivered_once_the_deny_stops_binding() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a deny of every principal, narrowed to the role, then removed";
    let (router, mut driven) = driven_emulator();
    let chain = given_chain(&router, case, &["sequence"]).await;
    let mapped = chain
        .queues
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: the chain holds one queue"));

    // The baseline: with nothing in force, the mapping consumes what it is sent.
    let baseline = "sequence-baseline";
    send_message(&router, case, &mapped.url, baseline, None).await;
    driven.poll_once().await;
    let delivered = held_item(&router, case, &chain.table, baseline).await;
    assert_ne!(
        delivered,
        Value::Null,
        "case {case:?}: with no policy on the queue the mapping must deliver {baseline:?}, and \
         the downstream table holds nothing for it"
    );

    // Denied to every principal: the message sent next stays queued, pass after pass, and
    // nothing downstream is written.
    let withheld = "sequence-withheld";
    let every = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": "*",
            "Action": receive_action(),
            "Resource": mapped.queue_arn.clone(),
        }],
    });
    set_queue_policy(&router, case, &mapped.url, Some(every)).await;
    send_message(&router, case, &mapped.url, withheld, None).await;
    for pass in 0..BLOCKED_PASSES {
        driven.poll_once().await;
        let counts = queue_message_counts(&router, case, &mapped.url).await;
        assert_eq!(
            counts,
            (1, 0, 0),
            "case {case:?}: while the queue denies its readers the receive, pass {pass} must \
             leave {withheld:?} visible on the queue, got {counts:?} as (visible, in flight, \
             delayed)"
        );
        let held = held_item(&router, case, &chain.table, withheld).await;
        assert_eq!(
            held,
            Value::Null,
            "case {case:?}: after pass {pass} the downstream table must hold nothing for the \
             withheld message, got {held}"
        );
    }

    // Narrowed to the execution role alone: that statement is about the calls the role itself
    // signs, not about the poller's reads, so the SAME message is handed over at the next pass
    // and the handler writes it.
    let role_only = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": { "AWS": chain.role_arn.clone() },
            "Action": receive_action(),
            "Resource": mapped.queue_arn.clone(),
        }],
    });
    set_queue_policy(&router, case, &mapped.url, Some(role_only)).await;
    driven.poll_once().await;
    let released = held_item(&router, case, &chain.table, withheld).await;
    assert_ne!(
        released,
        Value::Null,
        "case {case:?}: a deny aimed at the execution role alone withholds no read of the \
         poller's, so the withheld message must have been delivered, and the downstream table \
         holds nothing for it"
    );

    // And the policy removed outright decides the same way for the message sent after it.
    let after = "sequence-after-removal";
    let (status, cleared) = call_sqs(
        &router,
        "SetQueueAttributes",
        &json!({ "QueueUrl": mapped.url.clone(), "Attributes": { "Policy": "" } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: clearing the queue's policy must answer 200, got {status} {cleared}"
    );
    send_message(&router, case, &mapped.url, after, None).await;
    driven.poll_once().await;
    let last = held_item(&router, case, &chain.table, after).await;
    assert_ne!(
        last,
        Value::Null,
        "case {case:?}: with the policy removed the mapping must deliver {after:?}, and the \
         downstream table holds nothing for it"
    );
}
