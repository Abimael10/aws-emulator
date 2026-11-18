// SPDX-License-Identifier: CC0-1.0

//! How this service states the VALUE SET of `EventSourcePosition` when a request names a
//! `StartingPosition` the type does not admit.
//!
//! RULE: a value outside an enumerated type's set is refused with the modelled
//! `ValidationException` (HTTP 400) naming the rejected value, the member path and the whole
//! set that type admits — and the set is listed in the order AWS states it in, which is
//! neither the order the model declares the enum in nor ascending order. For
//! `EventSourcePosition` (`TRIM_HORIZON | LATEST | AT_TIMESTAMP` as the model declares it,
//! `smithy_query(query="shape", shape="EventSourcePosition")`) that order is
//! `[LATEST, AT_TIMESTAMP, TRIM_HORIZON]`.
//!
//! The rows differ along what the rule generalises over: WHICH value is rejected (another
//! vocabulary's word, a declared value in the wrong case, a declared value with trailing
//! space, one that is a prefix of a declared one) and WHICH event source the mapping names —
//! the order belongs to the enum SHAPE, so every member targeting it is stated alike. The
//! rows beside them are the control: a value the type does admit is accepted, so the
//! sentence is the answer to a value outside the set and not to the operation.
//!
//! Every row drives a real `CreateEventSourceMapping` through the production
//! `MultiServiceRouter` against a function this suite created, and reads back what the
//! refused call left behind.

use std::sync::Arc;

use serde_json::json;

use crate::event_source_mappings::{
    create_function, create_mapping, create_request, list_mappings, router_on, KINESIS_SOURCE,
    MSK_SOURCE,
};

/// The modelled error a value outside an enumerated type's set is refused with.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The member path the clause names, in the lowerCamel spelling the wire uses for it.
const MEMBER_PATH: &str = "startingPosition";

/// The order AWS lists `EventSourcePosition`'s value set in.
const STATED_SET: &str = "[LATEST, AT_TIMESTAMP, TRIM_HORIZON]";

/// The whole refusal a `StartingPosition` outside the type's set is answered with: the
/// envelope, the rejected value as the request sent it, the member path, and the set in the
/// order AWS states it.
fn enum_refusal(value: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{MEMBER_PATH}' failed to satisfy \
         constraint: Member must satisfy enum value set: {STATED_SET}"
    )
}

/// One row: the source the mapping would poll, the `StartingPosition` the request carries,
/// and whether the type admits it.
struct Row {
    case: &'static str,
    event_source: &'static str,
    starting_position: &'static str,
    admitted: bool,
}

/// The catalog. Each rejected value is a different way of being outside the set; each
/// admitted one is a value the model declares.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a cluster, with another vocabulary's word for the oldest record",
            event_source: MSK_SOURCE,
            starting_position: "EARLIEST",
            admitted: false,
        },
        Row {
            case: "a cluster, with a declared value in the wrong case",
            event_source: MSK_SOURCE,
            starting_position: "latest",
            admitted: false,
        },
        Row {
            case: "a cluster, with a declared value carrying trailing space",
            event_source: MSK_SOURCE,
            starting_position: "LATEST ",
            admitted: false,
        },
        Row {
            case: "a stream, with a prefix of a declared value",
            event_source: KINESIS_SOURCE,
            starting_position: "TRIM",
            admitted: false,
        },
        Row {
            case: "a stream, with another vocabulary's word",
            event_source: KINESIS_SOURCE,
            starting_position: "OLDEST",
            admitted: false,
        },
        Row {
            case: "a cluster, with the value for the newest records",
            event_source: MSK_SOURCE,
            starting_position: "LATEST",
            admitted: true,
        },
        Row {
            case: "a stream, with the value for the oldest records",
            event_source: KINESIS_SOURCE,
            starting_position: "TRIM_HORIZON",
            admitted: true,
        },
    ]
}

/// The members a mapping onto this source needs beside `EventSourceArn` and the position.
fn extras(event_source: &str, starting_position: &str) -> serde_json::Value {
    let mut extras = json!({ "StartingPosition": starting_position });
    if event_source == MSK_SOURCE {
        extras["Topics"] = json!(["order-events"]);
    }
    extras
}

#[tokio::test]
async fn a_starting_position_outside_the_set_is_refused_with_awss_own_ordering() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "esm-position-order-fn";
    create_function(&router, function).await;

    for row in rows() {
        let case = row.case;
        let answer = create_mapping(
            &router,
            &create_request(
                function,
                Some(row.event_source),
                &extras(row.event_source, row.starting_position),
            ),
        )
        .await;

        if row.admitted {
            assert_eq!(
                answer.status,
                202,
                "case {case:?}: a value the type admits must be accepted, got {} {}",
                answer.status,
                answer.text()
            );
            continue;
        }

        assert_eq!(
            answer.status,
            400,
            "case {case:?}: expected HTTP 400, got {} {}",
            answer.status,
            answer.text()
        );
        assert_eq!(
            answer.error_code(),
            VALIDATION_EXCEPTION,
            "case {case:?}: expected {VALIDATION_EXCEPTION}, got {}",
            answer.error_code()
        );
        let expected = enum_refusal(row.starting_position);
        let message = answer
            .string("message")
            .or_else(|| answer.string("Message"))
            .unwrap_or_default();
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // The refused rows persisted nothing: the mappings the function holds are the admitted
    // rows' alone.
    let admitted = rows().iter().filter(|row| row.admitted).count();
    let listed = list_mappings(&router, &format!("FunctionName={function}")).await;
    assert_eq!(
        listed.listed_uuids().len(),
        admitted,
        "expected only the admitted rows to have left a mapping behind, got {}",
        listed.text()
    );
}
