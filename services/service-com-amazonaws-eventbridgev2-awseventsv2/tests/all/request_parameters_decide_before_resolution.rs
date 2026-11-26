// SPDX-License-Identifier: CC0-1.0

//! What this service decides from the REQUEST before it looks at the resources the request
//! names, read where a client reads it: off the `rpcv2Cbor` wire.
//!
//! RULE 1 — a request parameter that is not valid is decided from the request ALONE, so it
//! is answered before the store is consulted: a request that is both malformed and
//! addressed at a resource that is not there is refused `400` `InvalidInputException` for
//! the parameter, never `404` for the resource, and nothing of it is written. The
//! complement holds too: the same request with valid parameters is refused for the resource
//! it names, so re-ordering the parameter readings takes nothing away from the not-found
//! path. `InvalidInputException` is the error this service's model declares for "A request
//! parameter is missing or not valid." (`smithy_query(query="operation",
//! operation="CreateEventSource,CreateSubscriber,UpdateSubscriber")` lists it on each of
//! them, `@httpError` 400, beside `ResourceNotFoundException`'s 404).
//!
//! RULE 2 — the idempotency token each create models (`ClientToken`, `@idempotencyToken`
//! with `@length` 1..64 and `@pattern` `^[\u0021-\u007E]+$`:
//! `smithy_query(query="shape", shape="ClientToken")`) is told apart by whether the request
//! states a VALUE in it. A request that states the member with a value of zero length
//! states no token at all, and that parameter is MISSING: the refusal is `400`
//! `InvalidInputException` whose whole `Message` is the fixed text of that class naming the
//! member, `ClientToken is required`. Its two neighbours stay as they are: a request that
//! OMITS the member is SERVED, because the model leaves it optional and the member's own doc
//! says "The SDK generates one automatically when the field is omitted"; and a value that
//! does carry characters the member's grammar excludes is a value that is not valid, stated
//! in AWS's constraint envelope like every other bad value. A create refused under this rule
//! mints nothing, whichever create it is, and the refusal is reached before the bus the
//! request names is resolved.
//!
//! The rows span what each rule generalises over: every operation of this service that both
//! validates a parameter of its own and resolves a resource the request names; the parameter
//! classes each of them reads (a pattern that is not a document, a pattern setting a
//! service-owned key, a filter configuration stating no filters); and, for the token, each of
//! the three creates, each class of stated value (empty, outside the grammar, past the length
//! maximum, usable) and the absence of the member.
//!
//! Nothing here waits on the wall clock: the one wait is for a bus to REPORT `ACTIVE`,
//! polled under a generous deadline by the shared harness.

use engine::InMemoryEngine;
use smithy_server_runtime::Operation;

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_event_source::CreateEventSource;
use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::list_event_buses::ListEventBuses;
use eventsv2::ops::list_event_sources::ListEventSources;
use eventsv2::ops::list_subscribers::ListSubscribers;
use eventsv2::ops::types::{
    AwsServiceEventsSourceConfiguration, CreateEventBusInput, CreateEventSourceInput,
    CreateSubscriberInput, EventSourceConfiguration, FilterConfiguration, InvokeConfiguration,
    ListEventBusesInput, ListEventSourcesInput, ListSubscribersInput, UpdateSubscriberInput,
};
use eventsv2::ops::update_subscriber::UpdateSubscriber;

use crate::modelled_input_refusal::{
    clause, constraint_envelope, encoded, must_match, refusal, rendered, unminted_arn, Wire,
    CLIENT_TOKEN_MAX, CLIENT_TOKEN_PATTERN, INVALID_INPUT_EXCEPTION, REGION,
    RESOURCE_NOT_FOUND_EXCEPTION,
};

/// The account the emulator mints ARNs under.
const ACCOUNT: &str = "000000000000";

/// The role a subscriber's `InvokeConfiguration` names, of the modelled `RoleArn` shape.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/event-delivery";

/// The target a subscriber's `InvokeConfiguration` delivers to, of the modelled `TargetArn`
/// shape.
const TARGET_ARN: &str = "arn:aws:sqs:us-west-2:000000000000:ordering-target";

/// The member each create models as its idempotency token, and the whole `Message` of the
/// refusal of a request that states it with no value in it.
///
/// Source: `smithy_query(query="operation",
/// operation="CreateEventBus,CreateEventSource,CreateSubscriber")` — every create's input
/// declares `ClientToken`; the text is the fixed wording of that refusal class, whose one
/// variable part is the member name.
const CLIENT_TOKEN_MEMBER: &str = "ClientToken";
const CLIENT_TOKEN_REQUIRED: &str = "ClientToken is required";

/// The dotted member path a constraint clause about that member names it by.
const CLIENT_TOKEN_PATH: &str = "clientToken";

/// The whole `Message` of the refusal of a forwarding `Pattern` that is not a JSON document.
/// Source: `smithy_query(query="doc", shape="AwsServiceEventsSourceConfiguration",
/// member="Pattern")` — "A filter pattern, as a JSON string".
const PATTERN_NOT_JSON: &str = "Pattern is not valid JSON";

/// The whole `Message` of the refusal of a filter configuration that states no filters.
/// Source: `smithy_query(query="shape", shape="FilterConfiguration")` — "On
/// CreateSubscriber, Filters is required and must contain at least one Filter with a
/// non-empty Pattern."
const FILTERS_REQUIRED: &str = "Filters is required";

/// The one language a `FilterConfiguration` may name.
/// Source: `smithy_query(query="shape", shape="FilterLanguage")`.
const FILTER_LANGUAGE: &str = "EVENT_BRIDGE_PATTERN";

// ---------------------------------------------------------------------------
// The requests the rows are built from
// ---------------------------------------------------------------------------

/// A `CreateEventBusInput` naming `name`, with `client_token` exactly as the row states it.
fn bus_create(name: &str, client_token: Option<&str>) -> CreateEventBusInput {
    CreateEventBusInput {
        name: name.to_string(),
        client_token: client_token.map(str::to_string),
        ..Default::default()
    }
}

/// A `CreateEventSourceInput` on `event_bus_arn` forwarding under `pattern`, with
/// `client_token` exactly as the row states it.
fn source_create(
    name: &str,
    event_bus_arn: &str,
    pattern: Option<&str>,
    client_token: Option<&str>,
) -> CreateEventSourceInput {
    CreateEventSourceInput {
        name: name.to_string(),
        event_bus_arn: event_bus_arn.to_string(),
        configuration: EventSourceConfiguration::AwsServiceEventsConfiguration(
            AwsServiceEventsSourceConfiguration {
                aws_service: "aws.s3".to_string(),
                pattern: pattern.map(str::to_string),
                on_failure_configuration: None,
            },
        ),
        description: None,
        tags: None,
        client_token: client_token.map(str::to_string),
    }
}

/// A `CreateSubscriberInput` on `event_bus_arn` carrying `filter_configuration`, with
/// `client_token` exactly as the row states it.
fn subscriber_create(
    name: &str,
    event_bus_arn: &str,
    filter_configuration: Option<FilterConfiguration>,
    client_token: Option<&str>,
) -> CreateSubscriberInput {
    CreateSubscriberInput {
        name: name.to_string(),
        event_bus_arn: event_bus_arn.to_string(),
        invoke_configuration: InvokeConfiguration {
            role_arn: ROLE_ARN.to_string(),
            target_arn: TARGET_ARN.to_string(),
            ..Default::default()
        },
        filter_configuration,
        client_token: client_token.map(str::to_string),
        ..Default::default()
    }
}

/// A `FilterConfiguration` that is NOT the empty shape — it names the language — and states
/// no filters, which is the one thing the shape's own documentation refuses.
fn filterless_configuration() -> FilterConfiguration {
    FilterConfiguration {
        language: Some(FILTER_LANGUAGE.to_string()),
        filters: None,
    }
}

/// A well-formed `SubscriberArn` this service never minted: the identifier segment is the 25
/// characters the modelled pattern pins.
/// Source: `smithy_query(query="shape", shape="SubscriberArn")`.
fn unminted_subscriber(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:subscriber/{name}/aaaaaaaaaaaaaaaaaaaaaaaaa")
}

// ---------------------------------------------------------------------------
// Reading the store back
// ---------------------------------------------------------------------------

/// A page size inside `MaxResults`' modelled `@range` that holds everything a case creates.
const WHOLE_PAGE: i32 = 50;

/// The names of every event bus this emulator holds.
async fn bus_names(wire: &Wire, case: &str) -> Vec<String> {
    wire.ok::<ListEventBuses<InMemoryEngine>>(
        case,
        &ListEventBusesInput {
            max_results: WHOLE_PAGE,
            ..Default::default()
        },
    )
    .await
    .event_buses
    .unwrap_or_default()
    .into_iter()
    .filter_map(|held| held.name)
    .collect()
}

/// The names of every event source on `bus`.
async fn source_names(wire: &Wire, case: &str, bus: &str) -> Vec<String> {
    wire.ok::<ListEventSources<InMemoryEngine>>(
        case,
        &ListEventSourcesInput {
            event_bus_arn: Some(bus.to_string()),
            max_results: WHOLE_PAGE,
            ..Default::default()
        },
    )
    .await
    .event_sources
    .unwrap_or_default()
    .into_iter()
    .filter_map(|held| held.name)
    .collect()
}

/// The names of every subscriber on `bus`.
async fn subscriber_names(wire: &Wire, case: &str, bus: &str) -> Vec<String> {
    wire.ok::<ListSubscribers<InMemoryEngine>>(
        case,
        &ListSubscribersInput {
            event_bus_arn: Some(bus.to_string()),
            max_results: WHOLE_PAGE,
            ..Default::default()
        },
    )
    .await
    .subscribers
    .unwrap_or_default()
    .into_iter()
    .filter_map(|held| held.name)
    .collect()
}

/// Assert that `status` and the body are the refusal `code` carrying exactly `message`.
fn assert_refused(case: &str, status: u16, body: &[u8], expected: u16, code: &str, message: &str) {
    assert_eq!(
        status, expected,
        "case {case:?}: expected HTTP {expected}, got {status} with {:?}",
        rendered(body)
    );
    let (served_code, served_message) = refusal(case, body);
    assert_eq!(
        served_code, code,
        "case {case:?}: expected {code}, got {served_code:?} with {served_message:?}"
    );
    assert_eq!(
        served_message, message,
        "case {case:?}: expected the whole Message {message:?}, got {served_message:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE 1: a bad parameter is decided before the resource the request names
// ---------------------------------------------------------------------------

/// RULE 1 over the operations that both read a parameter of their own and resolve a resource
/// the request names, and over the parameter classes each of them reads: a request that is
/// malformed AND addressed at a resource that is not there is refused for the PARAMETER.
#[tokio::test]
async fn a_bad_parameter_is_refused_before_the_resource_the_request_names_is_resolved() {
    let wire = Wire::new();
    let live = wire.active_bus("the bus the complements address", "ordering-bus").await;
    let absent_bus = unminted_arn(REGION, "no-such-bus", "zzzzzzzzzzzzzzzzzzzzzzzzz");
    let absent_subscriber = unminted_subscriber("no-such-subscriber");

    // (case, operation, the request body, the whole Message expected)
    let rows: Vec<(&str, &str, Vec<u8>, &str)> = vec![
        (
            "a forwarding pattern that is not a document, on a bus that does not exist",
            CreateEventSource::<InMemoryEngine>::NAME,
            encoded::<CreateEventSource<InMemoryEngine>>(&source_create(
                "unparseable-pattern-source",
                &absent_bus,
                Some("not a document"),
                Some("a-token"),
            )),
            PATTERN_NOT_JSON,
        ),
        (
            // A second parameter class of the same operation, so the rule is not keyed to
            // one reading: the key this source's forwarding rule sets itself.
            "a forwarding pattern setting a service-owned key, on a bus that does not exist",
            CreateEventSource::<InMemoryEngine>::NAME,
            encoded::<CreateEventSource<InMemoryEngine>>(&source_create(
                "reserved-key-source",
                &absent_bus,
                Some(r#"{"source":["demo.app"]}"#),
                Some("a-token"),
            )),
            "Pattern must not set the key 'source'; the forwarding rule sets it from the \
             requested AwsService and the source account and region",
        ),
        (
            "a filter configuration stating no filters, on a bus that does not exist",
            CreateSubscriber::<InMemoryEngine>::NAME,
            encoded::<CreateSubscriber<InMemoryEngine>>(&subscriber_create(
                "filterless-subscriber",
                &absent_bus,
                Some(filterless_configuration()),
                Some("a-token"),
            )),
            FILTERS_REQUIRED,
        ),
        (
            "a filter configuration stating no filters, on a subscriber that does not exist",
            UpdateSubscriber::<InMemoryEngine>::NAME,
            encoded::<UpdateSubscriber<InMemoryEngine>>(&UpdateSubscriberInput {
                subscriber_arn: absent_subscriber.clone(),
                filter_configuration: Some(filterless_configuration()),
                ..Default::default()
            }),
            FILTERS_REQUIRED,
        ),
    ];

    for (case, operation, body, expected) in rows {
        let (status, body) = wire.send(operation, body).await;
        assert_refused(case, status, &body, 400, INVALID_INPUT_EXCEPTION, expected);
    }

    // The complement: with its parameters valid, the SAME request is refused for the
    // resource it names, and that refusal names it.
    let complements: Vec<(&str, &str, Vec<u8>, &str)> = vec![
        (
            "a well-formed create naming a bus that does not exist",
            CreateEventSource::<InMemoryEngine>::NAME,
            encoded::<CreateEventSource<InMemoryEngine>>(&source_create(
                "orphan-source",
                &absent_bus,
                Some(r#"{"detail-type":["OrderPlaced"]}"#),
                Some("another-token"),
            )),
            "no-such-bus",
        ),
        (
            "a well-formed subscriber create naming a bus that does not exist",
            CreateSubscriber::<InMemoryEngine>::NAME,
            encoded::<CreateSubscriber<InMemoryEngine>>(&subscriber_create(
                "orphan-subscriber",
                &absent_bus,
                None,
                Some("another-token"),
            )),
            "no-such-bus",
        ),
        (
            "a well-formed update naming a subscriber that does not exist",
            UpdateSubscriber::<InMemoryEngine>::NAME,
            encoded::<UpdateSubscriber<InMemoryEngine>>(&UpdateSubscriberInput {
                subscriber_arn: absent_subscriber.clone(),
                description: Some("a description the update never applies".to_string()),
                ..Default::default()
            }),
            &absent_subscriber,
        ),
    ];

    for (case, operation, body, names) in complements {
        let (status, body) = wire.send(operation, body).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a request valid in every respect but the resource it names must \
             answer 404, got {status} with {:?}",
            rendered(&body)
        );
        let (code, message) = refusal(case, &body);
        assert_eq!(
            code, RESOURCE_NOT_FOUND_EXCEPTION,
            "case {case:?}: expected {RESOURCE_NOT_FOUND_EXCEPTION}, got {code:?} with \
             {message:?}"
        );
        assert!(
            message.contains(names),
            "case {case:?}: the refusal must name the resource the request addressed \
             ({names:?}), got {message:?}"
        );
    }

    // Nothing any refused request named was written: the live bus holds no source and no
    // subscriber of the refused names.
    let case = "what the refused requests left behind";
    let sources = source_names(&wire, case, &live).await;
    assert!(
        sources.is_empty(),
        "case {case:?}: a refused create must mint no event source, got {sources:?}"
    );
    let subscribers = subscriber_names(&wire, case, &live).await;
    assert!(
        subscribers.is_empty(),
        "case {case:?}: a refused create must mint no subscriber, got {subscribers:?}"
    );
}

/// RULE 1 where the resource the request names DOES resolve but is not yet usable: a
/// malformed request is still refused for its parameter, so the ordering is about the
/// request being read first and not about one not-found path.
#[tokio::test]
async fn a_bad_parameter_is_refused_whether_or_not_the_bus_it_names_resolves() {
    let wire = Wire::new();
    // A bus the create below addresses while it is still provisioning: whichever state it
    // has reached by then, the answer is the parameter refusal.
    let created = wire
        .ok::<CreateEventBus<InMemoryEngine>>(
            "the bus the create addresses",
            &bus_create("still-provisioning-bus", Some("a-bus-token")),
        )
        .await;
    let bus = created
        .event_bus_arn
        .expect("CreateEventBus must report an EventBusArn");

    let case = "a forwarding pattern that is not a document, on a bus that does resolve";
    let (status, body) = wire
        .call::<CreateEventSource<InMemoryEngine>>(&source_create(
            "unparseable-on-live-bus",
            &bus,
            Some("{"),
            Some("a-source-token"),
        ))
        .await;
    assert_refused(
        case,
        status,
        &body,
        400,
        INVALID_INPUT_EXCEPTION,
        PATTERN_NOT_JSON,
    );

    wire.await_active(case, &bus).await;
    let sources = source_names(&wire, case, &bus).await;
    assert!(
        sources.is_empty(),
        "case {case:?}: a refused create must mint no event source, got {sources:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE 2: a stated-but-empty idempotency token is a MISSING parameter
// ---------------------------------------------------------------------------

/// RULE 2 over every create of this service and over each class of value a request can state
/// for the token: empty (the parameter is missing), outside the member's grammar and past its
/// length maximum (the value is not valid), usable, and the member omitted altogether.
#[tokio::test]
async fn an_idempotency_token_stated_empty_is_refused_as_a_missing_parameter() {
    let wire = Wire::new();
    let bus = wire.active_bus("the host bus", "token-rule-bus").await;
    let too_long = "t".repeat(CLIENT_TOKEN_MAX + 1);
    let grammar = must_match(CLIENT_TOKEN_PATTERN);

    // (case, operation, the request body, the whole Message expected — empty for a request
    // this service SERVES)
    let rows: Vec<(&str, &str, Vec<u8>, String)> = vec![
        (
            "a bus create stating an empty token",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&bus_create("empty-token-bus", Some(""))),
            CLIENT_TOKEN_REQUIRED.to_string(),
        ),
        (
            "an event-source create stating an empty token",
            CreateEventSource::<InMemoryEngine>::NAME,
            encoded::<CreateEventSource<InMemoryEngine>>(&source_create(
                "empty-token-source",
                &bus,
                None,
                Some(""),
            )),
            CLIENT_TOKEN_REQUIRED.to_string(),
        ),
        (
            "a subscriber create stating an empty token",
            CreateSubscriber::<InMemoryEngine>::NAME,
            encoded::<CreateSubscriber<InMemoryEngine>>(&subscriber_create(
                "empty-token-subscriber",
                &bus,
                None,
                Some(""),
            )),
            CLIENT_TOKEN_REQUIRED.to_string(),
        ),
        (
            // The token is read before the bus the request names is resolved, so a request
            // that is wrong in both ways is refused for the token.
            "an event-source create stating an empty token on a bus that does not exist",
            CreateEventSource::<InMemoryEngine>::NAME,
            encoded::<CreateEventSource<InMemoryEngine>>(&source_create(
                "empty-token-orphan-source",
                &unminted_arn(REGION, "no-such-bus", "zzzzzzzzzzzzzzzzzzzzzzzzz"),
                None,
                Some(""),
            )),
            CLIENT_TOKEN_REQUIRED.to_string(),
        ),
        (
            // A value that DOES carry characters is a value that is not valid, and keeps
            // the constraint envelope: the two refusals are told apart.
            "a bus create stating a token outside the member's grammar",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&bus_create("spaced-token-bus", Some(" "))),
            constraint_envelope(&[clause(" ", CLIENT_TOKEN_PATH, &grammar)]),
        ),
        (
            "a bus create stating a token past the member's length maximum",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&bus_create(
                "long-token-bus",
                Some(&too_long),
            )),
            constraint_envelope(&[clause(
                &too_long,
                CLIENT_TOKEN_PATH,
                &format!("Member must have length less than or equal to {CLIENT_TOKEN_MAX}"),
            )]),
        ),
        (
            "a bus create stating a usable token",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&bus_create(
                "usable-token-bus",
                Some("a-usable-token"),
            )),
            String::new(),
        ),
        (
            "a bus create omitting the member altogether",
            CreateEventBus::<InMemoryEngine>::NAME,
            encoded::<CreateEventBus<InMemoryEngine>>(&bus_create("tokenless-bus", None)),
            String::new(),
        ),
    ];

    for (case, operation, body, expected) in rows {
        let (status, body) = wire.send(operation, body).await;
        if expected.is_empty() {
            assert_eq!(
                status, 200,
                "case {case:?}: {operation} must serve this request, got {status} with {:?}",
                rendered(&body)
            );
            continue;
        }
        assert_refused(case, status, &body, 400, INVALID_INPUT_EXCEPTION, &expected);
    }

    // Every refused create minted nothing, while the two served ones did: the names the
    // refusals carried are not in the store and the served ones are.
    let case = "what the refused creates left behind";
    let buses = bus_names(&wire, case).await;
    for refused in [
        "empty-token-bus",
        "spaced-token-bus",
        "long-token-bus",
    ] {
        assert!(
            !buses.contains(&refused.to_string()),
            "case {case:?}: the refused create of {refused:?} must mint no bus, got {buses:?}"
        );
    }
    for served in ["usable-token-bus", "tokenless-bus"] {
        assert!(
            buses.contains(&served.to_string()),
            "case {case:?}: the served create of {served:?} must mint its bus, got {buses:?}"
        );
    }
    let sources = source_names(&wire, case, &bus).await;
    assert!(
        sources.is_empty(),
        "case {case:?}: a create refused for its token must mint no event source, got \
         {sources:?}"
    );
    let subscribers = subscriber_names(&wire, case, &bus).await;
    assert!(
        subscribers.is_empty(),
        "case {case:?}: a create refused for its token must mint no subscriber, got \
         {subscribers:?}"
    );

    // And the member name a refusal states is the one the model declares, so the fixed text
    // and its one variable part cannot drift apart.
    assert_eq!(
        CLIENT_TOKEN_REQUIRED,
        format!("{CLIENT_TOKEN_MEMBER} is required"),
        "the refusal names the member the model declares"
    );
}
