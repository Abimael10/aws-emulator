// SPDX-License-Identifier: CC0-1.0

//! Whether the RESOURCE a mapping's `EventSourceArn` names actually EXISTS, for the event
//! source Lambda reaches by reading that resource first: an Amazon MQ broker.
//!
//! RULE: Lambda consumes an Amazon MQ source by connecting to the endpoints of the broker
//! the ARN names, which it reads off the broker itself, so `CreateEventSourceMapping` is held
//! to that broker existing in the service that OWNS brokers — a value naming a broker Amazon
//! MQ does not report is answered with the modelled `ResourceNotFoundException` (HTTP 404)
//! and nothing is stored, and a value naming one it does report is wired to it. The decision
//! is about the resource rather than about the shape of the request, so it is the same
//! whatever else the request states, and it follows the broker store as it stands at the
//! time of the call.
//!
//! The rows differ along what the rule generalises over: WHICH broker name the ARN carries
//! (one nothing created, one that extends a held broker's name, one a held broker's name
//! extends, a held name in another case, a long one, and a broker id mistaken for a name),
//! the broker id segment written beside it, and what else the request states (the queue and
//! the batch alone, and the source access configurations an Amazon MQ mapping authenticates
//! with). The sources whose own service is not the broker store — a queue Amazon SQS holds and
//! an MSK cluster — are still mapped, so what is resolved is the resource the ARN names rather
//! than every source being refused.
//!
//! Every row drives real HTTP requests through the router: the brokers are created through
//! Amazon MQ's own `CreateBroker` and read back through its `ListBrokers`, and the mappings
//! through `CreateEventSourceMapping`, `GetEventSourceMapping` and
//! `ListEventSourceMappings`. Every assertion names its case and the actual value.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_mapping_state, create_function, create_mapping, create_queue, create_request,
    get_mapping, given_broker, list_mappings, mq_call, refusal_message, router_on,
    router_with_queues, MSK_SOURCE, STATE_ENABLED,
};

/// The two brokers Amazon MQ holds in these suites, created through its own `CreateBroker`.
const HELD_BROKER: &str = "broker-resolution-held";
/// See [`HELD_BROKER`].
const HELD_BROKER_WITH_DIGITS: &str = "broker-resolution-42";

/// The modelled error a mapping onto a broker that does not exist is refused with.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// How Lambda words that refusal: fixed text naming the KIND of resource that is missing and
/// the request member that named it, rather than the value sent.
const ABSENT_BROKER_MESSAGE: &str =
    "The broker resource: eventSourceArn you requested does not exist.";

/// The secret a `BASIC_AUTH` source access configuration of an Amazon MQ mapping names — the
/// credentials Lambda connects to the broker with.
///
/// What the value carries is beside the point here: this suite is about resolving the BROKER
/// the `EventSourceArn` names, and the rows state the configuration only to show that the
/// broker's absence or presence decides the call whatever else the request carries. Whether
/// the secret itself resolves is its own question, decided by the service that owns secrets
/// and not mounted on these routers.
const BASIC_AUTH_SECRET: &str =
    "arn:aws:secretsmanager:us-east-1:000000000000:secret:mq-admin-AbCdEf";

/// The ARN of the broker `name` and `broker_id`, the shape "Amazon MQ – The ARN of the
/// broker" is written in (`broker:brokerName:brokerId`).
fn broker_source(name: &str, broker_id: &str) -> String {
    format!("arn:aws:mq:us-east-1:000000000000:broker:{name}:{broker_id}")
}

/// Every broker Amazon MQ reports, by name, read through its own `ListBrokers` — which is
/// what makes a broker absent here rather than anything Lambda holds.
async fn brokers_amazon_mq_reports(router: &MultiServiceRouter) -> Vec<String> {
    let listed = mq_call(router, "GET", "/v1/brokers", None).await;
    assert_eq!(
        listed.status,
        200,
        "ListBrokers must answer 200, got {} {}",
        listed.status,
        listed.text()
    );
    listed
        .json()
        .get("brokerSummaries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|summary| {
            summary
                .get("brokerName")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// One row of the absent-broker table: the case, the broker NAME the source ARN carries, the
/// source ARN the create states, and what else that request states.
struct AbsentRow {
    case: String,
    broker_name: String,
    event_source: String,
    extras: Value,
}

/// One row, built from the broker NAME it names and the broker id written beside it, so the
/// name the row asserts Amazon MQ does not hold and the ARN it sends cannot drift apart.
fn absent_row(case: &str, broker_name: &str, broker_id: &str, extras: &Value) -> AbsentRow {
    AbsentRow {
        case: case.to_string(),
        broker_name: broker_name.to_string(),
        event_source: broker_source(broker_name, broker_id),
        extras: extras.clone(),
    }
}

/// The rows: the broker name the ARN carries is the thing the rule generalises over, so each
/// row names a broker no `CreateBroker` created — including names that only ALMOST match one
/// that was — and the requests differ in the broker id written beside the name and in the
/// settings an Amazon MQ mapping carries beside its source.
fn absent_rows() -> Vec<AbsentRow> {
    let queue_and_batch = json!({ "Queues": ["testQueue"], "BatchSize": 1 });
    // The two configurations an Amazon MQ mapping connects with: the broker's credentials and
    // the virtual host inside it, from `SourceAccessConfiguration$Type`.
    let authenticated = json!({
        "Queues": ["testQueue"],
        "BatchSize": 1,
        "SourceAccessConfigurations": [
            { "Type": "BASIC_AUTH", "URI": BASIC_AUTH_SECRET },
            { "Type": "VIRTUAL_HOST", "URI": "/" },
        ],
    });
    vec![
        absent_row(
            "a broker name nothing created",
            "broker-resolution-ghost",
            "b-2222",
            &queue_and_batch,
        ),
        absent_row(
            "the same name under the all-zero broker id, with the broker's credentials",
            "broker-resolution-ghost",
            "b-00000000-0000-0000-0000-000000000000",
            &authenticated,
        ),
        absent_row(
            "a name that extends a held broker's name",
            &format!("{HELD_BROKER}-standby"),
            "b-3333",
            &queue_and_batch,
        ),
        absent_row(
            "a name a held broker's name extends",
            "broker-resolution",
            "b-4444",
            &authenticated,
        ),
        absent_row(
            "a held broker's name in upper case",
            &HELD_BROKER.to_uppercase(),
            "b-5555",
            &queue_and_batch,
        ),
        absent_row(
            "a long name nothing created",
            &format!("broker-resolution-{}", "absent-".repeat(4)),
            "b-6666",
            &queue_and_batch,
        ),
        absent_row(
            "a broker id written where the name belongs",
            "b-1111",
            "b-1111",
            &queue_and_batch,
        ),
    ]
}

/// RULE: a mapping onto an Amazon MQ broker that service does not hold is answered with the
/// modelled 404 and stores nothing — neither the source nor the function it named lists a
/// mapping afterwards.
#[tokio::test]
async fn a_mapping_onto_a_broker_amazon_mq_does_not_hold_is_refused_and_stores_nothing() {
    const FUNCTION: &str = "esm-broker-resolution-fn";

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, FUNCTION).await;
    given_broker(&router, HELD_BROKER).await;
    given_broker(&router, HELD_BROKER_WITH_DIGITS).await;

    // The brokers that DO exist are Amazon MQ's own answer, and none of the rows names one of
    // them: the absence every row is refused for is that service's state, not an assumption.
    let reported = brokers_amazon_mq_reports(&router).await;
    for held in [HELD_BROKER, HELD_BROKER_WITH_DIGITS] {
        assert!(
            reported.iter().any(|name| name == held),
            "case {held:?}: Amazon MQ must report the broker it created, got {reported:?}"
        );
    }

    for row in absent_rows() {
        let case = &row.case;
        let name = &row.broker_name;
        assert!(
            !reported.contains(name),
            "case {case:?}: Amazon MQ must report no broker named {name:?}, got {reported:?}"
        );

        let refused = create_mapping(
            &router,
            &create_request(FUNCTION, Some(&row.event_source), &row.extras),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code().contains(NOT_FOUND)),
            (404, true),
            "case {case:?}: {} names no broker, so the create must answer the modelled 404, \
             got {} {}",
            row.event_source,
            refused.status,
            refused.text()
        );
        assert_eq!(
            refusal_message(&refused).as_deref(),
            Some(ABSENT_BROKER_MESSAGE),
            "case {case:?}: the refusal names the kind of resource that is missing, got {}",
            refused.text()
        );

        // Nothing was stored: neither the source the request named nor the function it was
        // for lists a mapping. A listing by that ARN is a filter that matches nothing rather
        // than a refusal of its own.
        let by_source =
            list_mappings(&router, &format!("EventSourceArn={}", row.event_source)).await;
        assert_eq!(
            (by_source.status, by_source.listed_uuids()),
            (200, Vec::new()),
            "case {case:?}: the refused source must list no mapping, got {} {}",
            by_source.status,
            by_source.text()
        );
        let by_function = list_mappings(&router, &format!("FunctionName={FUNCTION}")).await;
        assert_eq!(
            (by_function.status, by_function.listed_uuids()),
            (200, Vec::new()),
            "case {case:?}: the function must list no mapping, got {} {}",
            by_function.status,
            by_function.text()
        );
    }
}

/// RULE, the other half: a source whose own service holds the resource the ARN names is wired
/// to it — the broker Amazon MQ minted an ARN for, whatever the mapping authenticates with,
/// and the sources whose own service is not the broker store at all.
#[tokio::test]
async fn a_mapping_onto_a_source_its_own_service_holds_is_wired_to_it() {
    const FUNCTION: &str = "esm-broker-held-fn";
    const CASE: &str = "the sources their own service holds";

    // Amazon SQS is mounted beside Lambda and Amazon MQ here, so the queue the control row maps
    // is a queue that service really holds and reports the ARN of.
    let router = router_with_queues();
    create_function(&router, FUNCTION).await;
    let held = given_broker(&router, HELD_BROKER).await;
    let held_with_digits = given_broker(&router, HELD_BROKER_WITH_DIGITS).await;
    let (_url, queue_arn) =
        create_queue(&router, CASE, "broker-resolution-intake", &json!({})).await;

    // (case, the source the create names, what else it states)
    let rows: Vec<(&str, String, Value)> = vec![
        (
            "the broker amazon mq minted this arn for",
            held,
            json!({ "Queues": ["testQueue"], "BatchSize": 1 }),
        ),
        (
            "another held broker, read with the broker's own credentials",
            held_with_digits,
            json!({
                "Queues": ["orders"],
                "BatchSize": 1,
                "SourceAccessConfigurations": [
                    { "Type": "BASIC_AUTH", "URI": BASIC_AUTH_SECRET },
                    { "Type": "VIRTUAL_HOST", "URI": "/" },
                ],
            }),
        ),
        (
            "a queue amazon sqs holds, whose own service is not the broker store",
            queue_arn,
            json!({}),
        ),
        (
            "an msk cluster, whose own service is not the broker store",
            MSK_SOURCE.to_string(),
            json!({ "Topics": ["orders"] }),
        ),
    ];

    for (case, event_source, extras) in rows {
        let created =
            create_mapping(&router, &create_request(FUNCTION, Some(&event_source), &extras)).await;
        assert_eq!(
            created.status,
            202,
            "case {case:?}: {event_source} is a source this account holds, so the create must \
             be accepted, got {} {}",
            created.status,
            created.text()
        );
        let uuid = created
            .string("UUID")
            .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", created.text()));
        let fetched = get_mapping(&router, &uuid).await;
        assert_eq!(
            (fetched.status, fetched.string("EventSourceArn")),
            (200, Some(event_source.clone())),
            "case {case:?}: the stored mapping reports the source it was created against, \
             got {} {}",
            fetched.status,
            fetched.text()
        );
        // The mapping the create stood up settles into the state in which it polls, so the
        // accepted row is a mapping that really exists rather than one answer.
        await_mapping_state(&router, case, &uuid, STATE_ENABLED).await;
    }
}

/// RULE, read over time: the answer follows the BROKER STORE as it stands at the time of the
/// call — the very request that was refused while nothing held the broker is accepted once
/// Amazon MQ holds it, and the mapping is then listed under that source.
#[tokio::test]
async fn the_broker_store_at_the_time_of_the_call_decides() {
    const FUNCTION: &str = "esm-broker-later-fn";
    const LATER_BROKER: &str = "broker-resolution-later";

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, FUNCTION).await;

    let extras = json!({ "Queues": ["testQueue"], "BatchSize": 1 });
    let before = create_mapping(
        &router,
        &create_request(
            FUNCTION,
            Some(&broker_source(LATER_BROKER, "b-7777")),
            &extras,
        ),
    )
    .await;
    assert_eq!(
        (before.status, before.error_code().contains(NOT_FOUND)),
        (404, true),
        "case {LATER_BROKER:?}: no broker of that name exists yet, so the create must answer \
         the modelled 404, got {} {}",
        before.status,
        before.text()
    );
    assert_eq!(
        refusal_message(&before).as_deref(),
        Some(ABSENT_BROKER_MESSAGE),
        "case {LATER_BROKER:?}: the refusal names the kind of resource that is missing, got {}",
        before.text()
    );

    let created_broker = given_broker(&router, LATER_BROKER).await;
    let after = create_mapping(
        &router,
        &create_request(FUNCTION, Some(&created_broker), &extras),
    )
    .await;
    assert_eq!(
        after.status,
        202,
        "case {LATER_BROKER:?}: the broker exists now, so the same mapping must be accepted, \
         got {} {}",
        after.status,
        after.text()
    );
    let uuid = after
        .string("UUID")
        .unwrap_or_else(|| panic!("case {LATER_BROKER:?}: no UUID in {}", after.text()));
    let listed = list_mappings(&router, &format!("EventSourceArn={created_broker}")).await;
    assert_eq!(
        (listed.status, listed.listed_uuids()),
        (200, vec![uuid]),
        "case {LATER_BROKER:?}: the accepted mapping must be listed under its source, got {} {}",
        listed.status,
        listed.text()
    );
}
