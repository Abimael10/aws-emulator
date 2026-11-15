// SPDX-License-Identifier: CC0-1.0

//! `LookupEvents`: the shape of a refusal body, and what a lookup carrying several
//! attributes is answered with.
//!
//! Every case goes over the wire through the shared [`super::emulator`] harness — an
//! `awsJson1_1` `POST /` dispatched by the same `MultiServiceRouter` the production router
//! serves — so what is asserted here is what a client sees: the HTTP status, the error code,
//! and the exact KEYS the JSON body carries.
//!
//! The rules under test:
//!
//! * every refusal of this operation is rendered from the error shape the model declares for
//!   it, so the body carries its description under the member that shape spells — `Message`,
//!   as `smithy_query(query="shape", shape="InvalidLookupAttributesException")` spells it,
//!   with no `jsonName` override — whether the request was refused before the handler ran
//!   (by the constraint walk, which finds an absent member while reading the request and a
//!   value outside a bound while walking it) or by the handler itself;
//! * a lookup is held to no bound on HOW MANY attributes it filters on —
//!   `LookupAttributesList` declares no `@length`
//!   (`smithy_query(query="shape", shape="LookupAttributesList")`) — so a well-formed
//!   request is SERVED whatever the length of that list: the events it reports are the ones
//!   satisfying every attribute stated, and a lookup that matches none is a 200 whose
//!   `Events` member is present and empty, since no modelled error covers "nothing
//!   matched".

use serde_json::{json, Value};

use super::emulator::{Emulator, HOME_REGION};

/// The operation under test.
const OPERATION: &str = "LookupEvents";

/// The HTTP status this awsJson1_1 API answers a client refusal with: the `@httpError` of
/// every error shape `LookupEvents` declares.
const REFUSED_STATUS: u16 = 400;

/// The member every error shape of this service carries its description in, spelled as the
/// model spells it.
const MESSAGE_MEMBER: &str = "Message";

/// The spelling of that member the framework's own pre-dispatch envelope uses, which no
/// answer of this service may carry: a client reading `Error.Message` finds nothing in it.
const GENERIC_MESSAGE_MEMBER: &str = "message";

/// CloudTrail's fixed sentence for a lookup attribute whose KEY it cannot read — absent,
/// empty, or outside the keys lookup supports.
const UNREADABLE_KEY: &str = "Lookup attribute key cannot be null or empty.";

/// CloudTrail's fixed sentence for a lookup attribute whose VALUE it cannot read — absent,
/// or empty.
const UNREADABLE_VALUE: &str = "Lookup attribute value cannot be null or empty.";

/// CloudTrail's fixed sentence for a lookup attribute that is otherwise not valid.
const INVALID_ATTRIBUTE: &str = "Lookup attribute key is invalid.";

/// CloudTrail's fixed sentence for an `EventCategory` outside the categories a lookup
/// selects on.
const INVALID_CATEGORY: &str = "Invalid event category specified.";

/// CloudTrail's fixed sentence for a lookup window whose start is after its end.
const INVERTED_WINDOW: &str = "The start time precedes the end time.";

/// An `AttributeValue` longer than the modelled maximum (`@length` 1..2000 on
/// `LookupAttributeValue`), which is the one past that published bound.
fn over_long_value() -> String {
    "v".repeat(2_001)
}

/// How many attributes one lookup filters on, at most: the bound the handler states
/// (`src/ops/lookup_events.rs`), far above the one item the member documents and the two a
/// recorded lookup was served for.
const MAX_ATTRIBUTES: usize = 64;

/// One refused lookup: the request, the modelled error it is answered with, and the
/// sentence that error class carries.
struct RefusalCase {
    case: &'static str,
    request: Value,
    code: &'static str,
    sentence: String,
}

/// A lookup stating `count` attributes, every one of which the recorded history satisfies
/// nothing for — used to drive the length bound, where what is under test is the LENGTH.
fn attributes_of_length(count: usize) -> Value {
    let attributes: Vec<Value> = (0..count)
        .map(|index| json!({"AttributeKey": "EventName", "AttributeValue": format!("Call{index}")}))
        .collect();
    json!({"LookupAttributes": attributes})
}

/// Every way a lookup is refused answers in the shape the model declares for it, so the
/// description reaches the client under the member that shape spells.
///
/// The rows span the two exits a refusal can leave through and the forms each takes: a
/// member absent from the request (found while the request is READ), a value outside a
/// modelled bound or value set (found while the request that was read is WALKED), and a
/// refusal only the handler can judge.
#[tokio::test]
async fn every_lookup_refusal_states_its_description_in_the_modelled_member() {
    let rows = vec![
        RefusalCase {
            case: "a key outside the modelled key set",
            request: json!({"LookupAttributes": [
                {"AttributeKey": "NotARealAttributeKey", "AttributeValue": "y"},
            ]}),
            code: "InvalidLookupAttributesException",
            sentence: UNREADABLE_KEY.to_string(),
        },
        RefusalCase {
            case: "a key differing from a modelled one only in case",
            request: json!({"LookupAttributes": [
                {"AttributeKey": "eventname", "AttributeValue": "CreateTrail"},
            ]}),
            code: "InvalidLookupAttributesException",
            sentence: UNREADABLE_KEY.to_string(),
        },
        RefusalCase {
            case: "the empty key",
            request: json!({"LookupAttributes": [
                {"AttributeKey": "", "AttributeValue": "CreateTrail"},
            ]}),
            code: "InvalidLookupAttributesException",
            sentence: UNREADABLE_KEY.to_string(),
        },
        RefusalCase {
            case: "no key at all, so the required member is absent",
            request: json!({"LookupAttributes": [{"AttributeValue": "CreateTrail"}]}),
            code: "InvalidLookupAttributesException",
            sentence: UNREADABLE_KEY.to_string(),
        },
        RefusalCase {
            case: "an unreadable key on the second attribute",
            request: json!({"LookupAttributes": [
                {"AttributeKey": "EventName", "AttributeValue": "CreateTrail"},
                {"AttributeKey": "EventRegion", "AttributeValue": HOME_REGION},
            ]}),
            code: "InvalidLookupAttributesException",
            sentence: UNREADABLE_KEY.to_string(),
        },
        RefusalCase {
            case: "a value below the modelled minimum length",
            request: json!({"LookupAttributes": [
                {"AttributeKey": "EventName", "AttributeValue": ""},
            ]}),
            code: "InvalidLookupAttributesException",
            sentence: UNREADABLE_VALUE.to_string(),
        },
        RefusalCase {
            case: "no value at all, so the required member is absent",
            request: json!({"LookupAttributes": [{"AttributeKey": "EventName"}]}),
            code: "InvalidLookupAttributesException",
            sentence: UNREADABLE_VALUE.to_string(),
        },
        RefusalCase {
            case: "a value one past the modelled maximum length",
            request: json!({"LookupAttributes": [
                {"AttributeKey": "EventName", "AttributeValue": over_long_value()},
            ]}),
            code: "InvalidLookupAttributesException",
            sentence: INVALID_ATTRIBUTE.to_string(),
        },
        RefusalCase {
            case: "a flag value outside the domain its key filters on",
            request: json!({"LookupAttributes": [
                {"AttributeKey": "ReadOnly", "AttributeValue": "maybe"},
            ]}),
            code: "InvalidLookupAttributesException",
            sentence: INVALID_ATTRIBUTE.to_string(),
        },
        RefusalCase {
            case: "an event category outside the modelled value set",
            request: json!({"EventCategory": "management"}),
            code: "InvalidEventCategoryException",
            sentence: INVALID_CATEGORY.to_string(),
        },
        RefusalCase {
            case: "an event category the model spells in another case",
            request: json!({"EventCategory": "Insight"}),
            code: "InvalidEventCategoryException",
            sentence: INVALID_CATEGORY.to_string(),
        },
        RefusalCase {
            case: "one attribute past the longest list a lookup filters on",
            request: attributes_of_length(MAX_ATTRIBUTES + 1),
            code: "InvalidLookupAttributesException",
            sentence: format!(
                "LookupAttributes carries {} attributes: a lookup filters on at most \
                 {MAX_ATTRIBUTES} attributes",
                MAX_ATTRIBUTES + 1
            ),
        },
        RefusalCase {
            case: "a window whose start is after its end",
            request: json!({"StartTime": 1_700_000_100, "EndTime": 1_700_000_000}),
            code: "InvalidTimeRangeException",
            sentence: INVERTED_WINDOW.to_string(),
        },
        RefusalCase {
            case: "a page size below the modelled minimum",
            request: json!({"MaxResults": 0}),
            code: "InvalidMaxResultsException",
            // The value is the one the request sent; the range is the one the refusal
            // states (`tests/all/page_size_and_query_id_refusals.rs` varies the value).
            sentence: "A max results value of 0 is invalid. Specify max results between 1 \
                       and 1000."
                .to_string(),
        },
    ];

    let emulator = Emulator::new();
    for row in rows {
        let case = row.case;
        let reply = emulator.call(OPERATION, &row.request).await;
        reply.expect_error(case, REFUSED_STATUS, row.code);
        let described = reply
            .body
            .get(MESSAGE_MEMBER)
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the modelled {MESSAGE_MEMBER} member in the \
                     error body, got {:?}",
                    reply.body
                )
            });
        assert!(
            reply.body.get(GENERIC_MESSAGE_MEMBER).is_none(),
            "case {case:?}: expected no generic {GENERIC_MESSAGE_MEMBER} key beside the \
             modelled one, got {:?}",
            reply.body
        );
        // Every class words its refusal, and the wording is the whole of it: the member
        // carries that class's sentence exactly.
        assert_eq!(
            described, row.sentence,
            "case {case:?}: expected this class's sentence, got {described:?}"
        );
    }
}

/// One lookup over a recorded event: which attributes it states, and whether the event
/// satisfies all of them.
struct FilterCase {
    case: &'static str,
    /// The attributes, each as the key and the value the request states.
    attributes: Vec<(&'static str, String)>,
    /// Whether the recorded event satisfies every one of them.
    selects: bool,
}

/// A lookup is served whatever number of attributes it carries, reports the events that
/// satisfy EVERY one of them, and answers an explicitly empty `Events` list when none does.
///
/// The rows differ along what the rule generalises over: how many attributes the list
/// carries (one to four), which keys they are, whether two of them name the same key, and
/// whether the recorded event agrees with all of them.
#[tokio::test]
async fn a_lookup_filters_on_every_attribute_it_carries() {
    let emulator = Emulator::new();
    let trail = "lookup-filter-trail";
    emulator
        .call(
            "CreateTrail",
            &json!({"Name": trail, "S3BucketName": "lookup-filter-bucket"}),
        )
        .await
        .expect_ok("CreateTrail");

    // The record of that call, read back through a single-attribute lookup: every row below
    // filters on the values this event itself reports, so what is varied is the FILTER and
    // not the event.
    let recorded = reported_events(
        &emulator,
        &json!({"LookupAttributes": [
            {"AttributeKey": "EventName", "AttributeValue": "CreateTrail"},
        ]}),
        "the record of the created trail",
    )
    .await;
    assert_eq!(
        recorded.len(),
        1,
        "the served CreateTrail must be recorded exactly once, got {recorded:?}"
    );
    let event = &recorded[0];
    let reported = |member: &str| -> String {
        event
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("the record must report {member}, got {event:?}"))
            .to_string()
    };
    let resource = |member: &str| -> String {
        event
            .get("Resources")
            .and_then(Value::as_array)
            .and_then(|resources| resources.first())
            .and_then(|resource| resource.get(member))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("the record must name a resource's {member}, got {event:?}"))
            .to_string()
    };

    let rows = vec![
        FilterCase {
            case: "one attribute the event satisfies",
            attributes: vec![("EventName", reported("EventName"))],
            selects: true,
        },
        FilterCase {
            case: "two attributes the event satisfies",
            attributes: vec![
                ("EventName", reported("EventName")),
                ("ReadOnly", reported("ReadOnly")),
            ],
            selects: true,
        },
        FilterCase {
            case: "three attributes the event satisfies",
            attributes: vec![
                ("EventName", reported("EventName")),
                ("EventSource", reported("EventSource")),
                ("ResourceName", resource("ResourceName")),
            ],
            selects: true,
        },
        FilterCase {
            case: "four attributes the event satisfies",
            attributes: vec![
                ("EventId", reported("EventId")),
                ("EventName", reported("EventName")),
                ("EventSource", reported("EventSource")),
                ("ResourceType", resource("ResourceType")),
            ],
            selects: true,
        },
        FilterCase {
            case: "two attributes, the second of which the event does not satisfy",
            attributes: vec![
                ("EventName", reported("EventName")),
                ("ReadOnly", flipped(&reported("ReadOnly"))),
            ],
            selects: false,
        },
        FilterCase {
            case: "two attributes naming the same key with different values",
            attributes: vec![
                ("EventName", reported("EventName")),
                ("EventName", "DeleteTrail".to_string()),
            ],
            selects: false,
        },
        FilterCase {
            case: "three attributes, the first of which nothing satisfies",
            attributes: vec![
                ("EventName", "NoSuchCall".to_string()),
                ("EventSource", reported("EventSource")),
                ("ReadOnly", reported("ReadOnly")),
            ],
            selects: false,
        },
        FilterCase {
            case: "the longest list a lookup filters on, every attribute satisfied",
            attributes: vec![("EventName", reported("EventName")); MAX_ATTRIBUTES],
            selects: true,
        },
        FilterCase {
            case: "one attribute naming a resource nothing addressed",
            attributes: vec![("ResourceName", "no-such-trail".to_string())],
            selects: false,
        },
    ];

    let event_id = reported("EventId");
    for row in rows {
        let case = row.case;
        let attributes: Vec<Value> = row
            .attributes
            .iter()
            .map(|(key, value)| json!({"AttributeKey": key, "AttributeValue": value}))
            .collect();
        let events = reported_events(&emulator, &json!({"LookupAttributes": attributes}), case)
            .await;
        let ids: Vec<String> = events
            .iter()
            .map(|event| {
                event
                    .get("EventId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            ids.contains(&event_id),
            row.selects,
            "case {case:?}: expected the recorded event to be selected: {}, got {ids:?}",
            row.selects
        );
        if !row.selects {
            assert!(
                events.is_empty(),
                "case {case:?}: expected an empty page, got {events:?}"
            );
        }
    }
}

/// The events a served lookup reports, which pins that `Events` is PRESENT on a success —
/// an empty list is emitted, not omitted, since nothing a request can state turns a
/// matching-nothing lookup into a refusal.
async fn reported_events(emulator: &Emulator, body: &Value, case: &str) -> Vec<Value> {
    let reply = emulator.call(OPERATION, body).await;
    let answered = reply.expect_ok(case);
    answered
        .get("Events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected an Events list in the answer, got {answered:?}")
        })
}

/// The other value the `ReadOnly` attribute takes: an event reports that flag as `true` or
/// `false`, so this is the value the recorded event does NOT carry.
fn flipped(read_only: &str) -> String {
    match read_only {
        "true" => "false".to_string(),
        _ => "true".to_string(),
    }
}
