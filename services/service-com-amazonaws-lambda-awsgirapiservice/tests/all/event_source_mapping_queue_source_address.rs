// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how `CreateEventSourceMapping` resolves an Amazon SQS event source:
//! "Creates a mapping between an event source and an Lambda Event Function. Lambda reads
//! items from the event source and invokes the function."
//!
//! The rule under test: a mapping onto a queue is wired up by READING that queue, so Lambda
//! does not judge the account FIELD of the `EventSourceArn` — it resolves the ARN to the
//! queue's address, an http(s) url whose path is `/<account id of the ARN>/<queue name of the
//! ARN>`, and refuses the mapping with the failure the queue service answered that address
//! with, reported as the read it came from. An address of an account this endpoint does not
//! answer for is served by nothing here, so such a mapping is refused with the modelled
//! `InvalidParameterValueException` (HTTP 400) carrying
//! `Error occurred while ReceiveMessage. SQS Error Code: InvalidAddress. SQS Error Message:
//! The address <queue url> is not valid for this endpoint.` — everything outside the url is
//! fixed text, and the url is the one the ARN resolves to — and no mapping is stored. Every
//! OTHER kind of event source is one of this account's own resources, the one documented
//! cross-account form (an Amazon MSK VPC connection) aside, and is not resolved to a queue
//! address at all.
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter` with
//! Lambda and Amazon SQS on ONE engine, and the queue of the accepted rows is created through
//! Amazon SQS exactly as a client creates it, so a mapping answered but never stored, or
//! refused against anything other than the address, fails here. The rows differ along what
//! the rule generalises over: WHICH account the ARN carries (a fabricated one, the example id
//! AWS writes its ARN formats with, one a single digit from this account's, and none at all),
//! whether a queue of that name exists in this account, the FORM of the queue name (plain,
//! dotted, FIFO) and — for the boundary — which source service the ARN names.

use serde_json::{json, Value};

use crate::event_source_mappings::{
    create_function, create_mapping, create_queue, get_mapping, list_mappings, router_with_queues,
    Answer,
};

/// The wire code of the modelled refusal this rule answers with, from
/// `smithy_query(query="operation", operation="CreateEventSourceMapping")`:
/// `InvalidParameterValueException`, errorKind client, httpError 400.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The fixed text before the address in the propagated refusal: the read Lambda attempted on
/// the source, and the error code the queue service answered it with.
const REFUSAL_PREFIX: &str = "Error occurred while ReceiveMessage. SQS Error Code: \
                              InvalidAddress. SQS Error Message: The address ";

/// The fixed text after it.
const REFUSAL_SUFFIX: &str = " is not valid for this endpoint.";

/// The region the harness signs for, which is the one the ARNs under test carry: a queue
/// belongs to one Region's namespace.
const REGION: &str = "us-east-1";

/// The `State` a freshly created mapping carries
/// (`EventSourceMappingConfiguration$State`).
const STATE_CREATING: &str = "Creating";

/// The message a refusal carries, however the wire spells the member.
fn message_of(answer: &Answer) -> String {
    answer
        .string("message")
        .or_else(|| answer.string("Message"))
        .unwrap_or_default()
}

/// The PATH of an http(s) url — everything from the `/` that ends its authority — and `None`
/// for a value that is no http(s) url at all.
fn path_of(url: &str) -> Option<&str> {
    let authority = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let path = authority.find('/')?;
    Some(&authority[path..])
}

/// One row of the refused catalogue: the account id and the queue name the `EventSourceArn`
/// carries, and whether a queue of that name is created in THIS account first.
struct Row {
    case: &'static str,
    account: &'static str,
    queue: &'static str,
    exists_here: bool,
}

/// The catalogue: an account that is not the one this endpoint answers for, written four
/// ways, over queue names of three forms, with and without a queue of that name in this
/// account — the account field is not what decides, so a name that really exists here is
/// refused exactly like one nothing holds.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a fabricated account, naming a queue that exists here",
            account: "111122223333",
            queue: "order-intake",
            exists_here: true,
        },
        Row {
            case: "the example account id AWS writes its ARN formats with",
            account: "123456789012",
            queue: "order-overflow",
            exists_here: false,
        },
        Row {
            case: "an account one digit from this one, naming a queue that exists here",
            account: "000000000001",
            queue: "order-returns",
            exists_here: true,
        },
        Row {
            case: "another account's fifo queue",
            account: "999999999999",
            queue: "order-intake.fifo",
            exists_here: false,
        },
        Row {
            case: "another account's queue whose name carries dots and dashes",
            account: "210987654321",
            queue: "order.intake-v2",
            exists_here: false,
        },
        Row {
            case: "an arn whose account segment is empty",
            account: "",
            queue: "order-intake",
            exists_here: true,
        },
    ]
}

#[tokio::test]
async fn a_queue_of_another_account_is_refused_with_the_address_it_resolves_to() {
    let router = router_with_queues();
    let function = "esm-address-fn";
    create_function(&router, function).await;

    for row in rows() {
        let case = row.case;
        if row.exists_here {
            // The same NAME, in the account this endpoint answers for: the refusal is about
            // the address the ARN resolves to, not about a queue nobody created.
            create_queue(&router, case, row.queue, &json!({})).await;
        }
        let event_source = format!("arn:aws:sqs:{REGION}:{}:{}", row.account, row.queue);

        let answer = create_mapping(
            &router,
            &json!({ "FunctionName": function, "EventSourceArn": event_source }),
        )
        .await;

        assert_eq!(
            answer.status,
            400,
            "case {case:?}: {event_source} addresses a queue this endpoint does not serve, so \
             the create must be refused with HTTP 400, got {} {}",
            answer.status,
            answer.text()
        );
        assert!(
            answer.error_code().contains(INVALID_PARAMETER),
            "case {case:?}: expected the code {INVALID_PARAMETER}, got {} in {}",
            answer.error_code(),
            answer.text()
        );
        let message = message_of(&answer);
        let address = message
            .strip_prefix(REFUSAL_PREFIX)
            .and_then(|rest| rest.strip_suffix(REFUSAL_SUFFIX));
        let Some(address) = address else {
            panic!(
                "case {case:?}: expected the message {REFUSAL_PREFIX}<queue url>\
                 {REFUSAL_SUFFIX}, got {message:?}"
            );
        };
        // The one variable part is the queue's own address, so the mapping's refusal names
        // which address to go look at: an http(s) url addressing the account and the queue
        // name the ARN carried.
        let expected_path = format!("/{}/{}", row.account, row.queue);
        assert_eq!(
            path_of(address),
            Some(expected_path.as_str()),
            "case {case:?}: expected the refusal to name an http(s) url with the path \
             {expected_path:?}, got {address:?} in {message:?}"
        );

        // The refusal is INSTEAD of the mapping: nothing is wired onto that source.
        let listed = list_mappings(&router, "").await;
        let sources: Vec<String> = listed
            .mappings()
            .iter()
            .filter_map(|mapping| {
                mapping
                    .get("EventSourceArn")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        assert!(
            !sources.contains(&event_source),
            "case {case:?}: a refused create must leave no mapping on {event_source}, got {}",
            listed.text()
        );
    }
}

#[tokio::test]
async fn a_queue_of_this_account_is_mapped_onto_the_address_it_resolves_to() {
    let router = router_with_queues();
    let case = "a queue of this account, created through Amazon SQS";
    let function = "esm-address-own-fn";
    create_function(&router, function).await;
    // The ARN comes out of the queue service itself rather than being spelled here, so the
    // mapping is made on the address that queue really answers at.
    let (_, queue_arn) = create_queue(&router, case, "order-intake", &json!({})).await;

    let answer = create_mapping(
        &router,
        &json!({ "FunctionName": function, "EventSourceArn": queue_arn }),
    )
    .await;
    assert_eq!(
        answer.status,
        202,
        "case {case:?}: {queue_arn} addresses a queue this endpoint serves, so the create must \
         be accepted, got {} {}",
        answer.status,
        answer.text()
    );
    let uuid = answer
        .string("UUID")
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", answer.text()));

    // Round trip: the accepted mapping is a record this emulator holds, on that queue.
    let reading = get_mapping(&router, &uuid).await;
    assert_eq!(
        (
            reading.status,
            reading.string("EventSourceArn"),
            reading.string("State"),
        ),
        (
            200,
            Some(queue_arn.clone()),
            Some(STATE_CREATING.to_string()),
        ),
        "case {case:?}: the mapping must read back on {queue_arn}, got {} {}",
        reading.status,
        reading.text()
    );
}

#[tokio::test]
async fn only_a_queue_source_is_resolved_to_a_queue_address() {
    let router = router_with_queues();
    let function = "esm-address-other-sources-fn";
    create_function(&router, function).await;

    // (case, the source ARN, the extras that source's mapping states, whether it is accepted)
    let rows: Vec<(&str, String, Value, bool)> = vec![
        (
            "a data stream of another account",
            format!("arn:aws:kinesis:{REGION}:111122223333:stream/order-events"),
            json!({ "StartingPosition": "LATEST" }),
            false,
        ),
        (
            "a table's stream of another account",
            format!(
                "arn:aws:dynamodb:{REGION}:111122223333:table/orders/stream/\
                 2026-10-23T11:46:49.000"
            ),
            json!({ "StartingPosition": "LATEST" }),
            false,
        ),
        (
            // "Amazon Managed Streaming for Apache Kafka – the ARN of the VPC connection
            // (for cross-account event source mappings)": the ONE cross-account form the
            // `EventSourceArn` member documents.
            "an msk vpc connection of another account, the documented cross-account form",
            format!(
                "arn:aws:kafka:{REGION}:111122223333:vpc-connection/\
                 11111111-2222-3333-4444-555555555555-1/22222222-3333-4444-5555-666666666666"
            ),
            json!({ "Topics": ["orders"] }),
            true,
        ),
    ];

    for (case, event_source, extras, accepted) in rows {
        let mut body = json!({ "FunctionName": function, "EventSourceArn": event_source });
        if let (Some(body), Some(extras)) = (body.as_object_mut(), extras.as_object()) {
            for (member, value) in extras {
                body.insert(member.clone(), value.clone());
            }
        }
        let answer = create_mapping(&router, &body).await;
        if accepted {
            assert_eq!(
                answer.status,
                202,
                "case {case:?}: {event_source} is the documented cross-account source, so the \
                 create must be accepted, got {} {}",
                answer.status,
                answer.text()
            );
            continue;
        }
        assert_eq!(
            answer.status,
            400,
            "case {case:?}: {event_source} names no source this account can map, so the create \
             must be refused with HTTP 400, got {} {}",
            answer.status,
            answer.text()
        );
        // A source that is no queue is not read through a queue address, so its refusal is
        // not the propagated queue read at all.
        let message = message_of(&answer);
        assert!(
            !message.starts_with(REFUSAL_PREFIX),
            "case {case:?}: {event_source} is no queue, so the refusal must not report a queue \
             read, got {message:?}"
        );
    }
}
