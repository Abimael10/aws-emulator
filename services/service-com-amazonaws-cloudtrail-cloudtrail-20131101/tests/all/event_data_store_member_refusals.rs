// SPDX-License-Identifier: CC0-1.0

//! What an `EventDataStore` member a request carries is REFUSED with, over the
//! operations that take one — driven end to end over the wire through the shared harness
//! (`tests/all/emulator.rs`), so what is asserted is what a client sees: the HTTP status,
//! the error code and the sentence.
//!
//! The two rules under test:
//!
//! * **the event-configuration pair judges the member it is given as its own parameter.**
//!   A value that is neither an event data store ARN nor the bare id suffix of one names
//!   no store in any account, so the call is refused on the FORM of the value rather than
//!   reported as an absent store: `PutEventConfiguration`, which validates the member as
//!   a parameter of its own request, answers the modelled `InvalidParameterException`,
//!   while the three operations that only READ or SWITCH the store it names
//!   (`GetEventConfiguration`, `EnableFederation`, `DisableFederation`) answer the
//!   modelled `EventDataStoreARNInvalidException`. A value in a form the service CAN
//!   read, naming a store the registry does not hold, is the absent store all four report
//!   (`EventDataStoreNotFoundException`).
//!
//! * **an operation whose `EventDataStore` member the model marks deprecated resolves on
//!   its QUERY selector.** `DescribeQueryRequest$EventDataStore` and
//!   `CancelQueryRequest$EventDataStore` are deprecated ("EventDataStore is no longer
//!   required by …", `smithy_query(query="operation",operation="DescribeQuery,CancelQuery")`),
//!   so the store named only NARROWS which store's query the request addresses: a selector
//!   that matches no query it addresses is `QueryIdNotFoundException`, whether or not the
//!   store named exists, and only a value the service cannot read as a store at all is
//!   refused as an invalid ARN. `GetQueryResults` reads its member the same way.
//!
//! Both not-found refusals answer HTTP 400, the status CloudTrail answers with rather
//! than the `@httpError(404)` its model annotates.
//!
//! `StartQuery` and `CreateEventDataStore` can mint neither a query nor a store here, so
//! the rows seed the SAME registries the handlers read (the harness's `state()`) and then
//! go over the wire for every assertion.

use serde_json::{json, Value};
use std::time::{Duration, SystemTime};

use service_com_amazonaws_cloudtrail_cloudtrail_20131101::state::{
    self, event_data_store_arn, EventDataStoreRecord, EventDataStoreState, QueryRecord, QueryState,
};

use super::emulator::{Emulator, HOME_REGION};

/// The Region no case calls in, for the ARN forms that pin a store elsewhere.
const OTHER_REGION: &str = "eu-west-1";

/// The HTTP status this service answers a not-found refusal with.
const NOT_FOUND_STATUS: u16 = 400;

/// The store every row that needs a stored one addresses. A store id is the resource
/// suffix of its ARN, in the `UUID` form the model declares for one.
const STORE_ID: &str = "aaaaaaaa-1111-2222-3333-444444444444";

/// A store id in the same form that no case seeds.
const ABSENT_STORE_ID: &str = "bbbbbbbb-1111-2222-3333-444444444444";

/// An instant, as a fixed offset from the epoch.
fn at(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

/// Seed one event data store in `region`, the way `CreateEventDataStore` records one.
fn seed_store(emulator: &Emulator, id: &str, region: &str) {
    emulator.state().lock().event_data_stores.insert(
        id.to_string(),
        EventDataStoreRecord {
            id: id.to_string(),
            name: format!("store-{id}"),
            region: region.to_string(),
            status: EventDataStoreState::Enabled,
            created_at: Some(at(1_700_000_000)),
            ..EventDataStoreRecord::default()
        },
    );
}

/// Seed one query run on `store_id` in [`HOME_REGION`], in `status`, the way `StartQuery`
/// mints one.
fn seed_query(emulator: &Emulator, query_id: &str, store_id: &str, status: QueryState) {
    emulator.state().lock().queries.insert(
        query_id.to_string(),
        QueryRecord {
            query_id: query_id.to_string(),
            event_data_store_id: store_id.to_string(),
            region: HOME_REGION.to_string(),
            query_string: format!("SELECT eventName FROM {store_id} LIMIT 1"),
            status,
            created_at: Some(at(1_700_000_100)),
            // A run that is settled owes the clock nothing, so its status is the one
            // every read of it reports.
            settles_at: None,
            ..QueryRecord::default()
        },
    );
}

/// A `UUID` is exactly 36 characters of `[a-f0-9-]`, which is what a query id has to be
/// for the boundary validator to let the request through to the handler.
fn query_id(nth: u8) -> String {
    format!("{nth:02x}cccccc-1111-2222-3333-444444444444")
}

/// The values no operation of this service can read as an event data store identifier,
/// each in a different way of failing to be one: the catalog is the two forms the member
/// documents ("The ARN (or the ID suffix of the ARN) of an event data store"), so a row is
/// a way of being neither.
fn unreadable_identifiers() -> Vec<(&'static str, String)> {
    vec![
        (
            "another service's ARN",
            format!("arn:aws:logs:{HOME_REGION}:000000000000:eventdatastore/{STORE_ID}"),
        ),
        (
            "a CloudTrail ARN whose resource is not a store at all",
            format!("arn:aws:cloudtrail:{HOME_REGION}:000000000000:trail/some-trail"),
        ),
        (
            "an ARN carrying no Region",
            format!("arn:aws:cloudtrail::000000000000:eventdatastore/{STORE_ID}"),
        ),
        (
            "an ARN whose resource suffix is empty",
            format!("arn:aws:cloudtrail:{HOME_REGION}:000000000000:eventdatastore/"),
        ),
        ("a bare value in no id form", "not-an-arn-at-all".to_string()),
    ]
}

/// A CloudTrail event data store ARN whose resource id is not an id this service could
/// have minted — the form the federation and event-configuration operations judge as well,
/// a store id being the `UUID` the model declares for one.
fn arn_with_a_resource_id_no_store_carries() -> String {
    format!("arn:aws:cloudtrail:{HOME_REGION}:000000000000:eventdatastore/not-an-arn-at-all")
}

/// The values the federation and event-configuration quartet refuses on the FORM of the
/// member: every value no operation can read, plus the store ARN whose resource id is not
/// a store id.
fn unusable_identifiers() -> Vec<(&'static str, String)> {
    let mut rows = unreadable_identifiers();
    rows.push((
        "a CloudTrail store ARN whose resource id is not a store id",
        arn_with_a_resource_id_no_store_carries(),
    ));
    rows
}

/// RULE: a value the service cannot read as an event data store is refused on the form of
/// the value — as the modelled `InvalidParameterException` by `PutEventConfiguration`,
/// which validates the member as a parameter of its own request, and as the modelled
/// `EventDataStoreARNInvalidException` by the three operations that read or switch the
/// store the member names.
///
/// The rows differ in HOW the value fails to be an identifier ([`unusable_identifiers`]),
/// which is what the rule generalises over, and the last block holds the contrast: a value
/// the service CAN read, naming a store the registry does not hold, is the absent store
/// all four report.
#[tokio::test]
async fn an_unusable_event_data_store_member_is_refused_on_its_form() {
    let emulator = Emulator::new();
    seed_store(&emulator, STORE_ID, HOME_REGION);

    // The configuration members `PutEventConfiguration` requires of a store-level call,
    // so every row reaches the member under test rather than the request's own shape.
    let configuration = json!({"MaxEventSize": "Standard", "ContextKeySelectors": []});
    let role = format!("arn:aws:iam::{}:role/federation", engine::emulator_account_id());

    for (case, identifier) in unusable_identifiers() {
        // The three operations that READ or SWITCH the store the member names.
        for operation in ["GetEventConfiguration", "EnableFederation", "DisableFederation"] {
            let mut request = json!({"EventDataStore": identifier});
            if operation == "EnableFederation" {
                request["FederationRoleArn"] = json!(role);
            }
            let case = format!("{operation} on {case}");
            let reply = emulator.call(operation, &request).await;
            reply.expect_error(&case, 400, "EventDataStoreARNInvalidException");
            let expected = state::event_data_store_arn_invalid_message(&identifier, HOME_REGION);
            assert_eq!(
                reply.message(),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                reply.message()
            );
        }

        // The operation that validates the member as a parameter of its own request.
        let mut request = configuration.clone();
        request["EventDataStore"] = json!(identifier);
        let case = format!("PutEventConfiguration on {case}");
        let reply = emulator.call("PutEventConfiguration", &request).await;
        reply.expect_error(&case, 400, "InvalidParameterException");
        assert_eq!(
            reply.message(),
            state::UNUSABLE_EVENT_DATA_STORE_MESSAGE,
            "case {case:?}: expected {:?}, got {:?}",
            state::UNUSABLE_EVENT_DATA_STORE_MESSAGE,
            reply.message()
        );
    }

    // The contrast: a readable identifier naming a store the registry does not hold is
    // the absent store, reported the same way by all four — including the id form, and
    // the stored store's id addressed from another Region.
    for (case, identifier, addressed) in [
        (
            "an absent store by its bare id",
            ABSENT_STORE_ID.to_string(),
            event_data_store_arn(HOME_REGION, ABSENT_STORE_ID),
        ),
        (
            "an absent store by its ARN",
            event_data_store_arn(HOME_REGION, ABSENT_STORE_ID),
            event_data_store_arn(HOME_REGION, ABSENT_STORE_ID),
        ),
        (
            "the stored store addressed from another Region",
            event_data_store_arn(OTHER_REGION, STORE_ID),
            event_data_store_arn(OTHER_REGION, STORE_ID),
        ),
    ] {
        for operation in [
            "GetEventConfiguration",
            "EnableFederation",
            "DisableFederation",
            "PutEventConfiguration",
        ] {
            let mut request = if operation == "PutEventConfiguration" {
                configuration.clone()
            } else {
                json!({})
            };
            request["EventDataStore"] = json!(identifier);
            if operation == "EnableFederation" {
                request["FederationRoleArn"] = json!(role);
            }
            let case = format!("{operation} on {case}");
            let reply = emulator.call(operation, &request).await;
            reply.expect_error(&case, NOT_FOUND_STATUS, "EventDataStoreNotFoundException");
            let expected = state::event_data_store_not_found_message(&addressed);
            assert_eq!(
                reply.message(),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                reply.message()
            );
        }
    }
}

/// RULE: an operation whose `EventDataStore` member the model marks deprecated resolves
/// on its query selector — the store named only narrows which store's query the request
/// addresses — so a selector matching no query it addresses is `QueryIdNotFoundException`
/// whatever the store member says, and the store is never what such a call is refused on.
///
/// The rows differ in the STORE the request names (none, an absent id, an absent store's
/// ARN, the stored store's id pinned to another Region) and in the OPERATION, which is
/// what the rule generalises over; the stored query under the stored store is the row
/// that resolves.
#[tokio::test]
async fn a_deprecated_store_member_never_decides_a_query_refusal() {
    let emulator = Emulator::new();
    seed_store(&emulator, STORE_ID, HOME_REGION);
    let stored_query = query_id(1);
    seed_query(&emulator, &stored_query, STORE_ID, QueryState::Finished);
    let unknown_query = query_id(9);

    // (case, the value of the deprecated member, whether it narrows to the stored store)
    let store_members: Vec<(&str, Option<String>, bool)> = vec![
        ("no store member at all", None, true),
        ("the stored store by its bare id", Some(STORE_ID.to_string()), true),
        (
            "the stored store by its ARN",
            Some(event_data_store_arn(HOME_REGION, STORE_ID)),
            true,
        ),
        (
            "an absent store by its bare id",
            Some(ABSENT_STORE_ID.to_string()),
            false,
        ),
        (
            "an absent store by its ARN",
            Some(event_data_store_arn(HOME_REGION, ABSENT_STORE_ID)),
            false,
        ),
        (
            "the stored store's id pinned to another Region",
            Some(event_data_store_arn(OTHER_REGION, STORE_ID)),
            false,
        ),
    ];

    for (row, (member_case, member, narrows_to_the_stored_store)) in
        store_members.into_iter().enumerate()
    {
        for (column, operation) in ["DescribeQuery", "GetQueryResults", "CancelQuery"]
            .into_iter()
            .enumerate()
        {
            // An id naming no query at all: the QUERY is what the caller is answered
            // about, whatever the store member carries.
            let mut request = json!({"QueryId": unknown_query});
            if let Some(member) = &member {
                request["EventDataStore"] = json!(member);
            }
            let case = format!("{operation} on an unknown query id with {member_case}");
            let reply = emulator.call(operation, &request).await;
            reply.expect_error(&case, NOT_FOUND_STATUS, "QueryIdNotFoundException");
            let expected = state::query_not_found_message("QueryId", &unknown_query);
            assert_eq!(
                reply.message(),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                reply.message()
            );

            // A STORED query: it resolves exactly when the store member narrows to the
            // store it ran on, and is not found when the member narrows to another.
            // `CancelQuery` cancels what it resolves, so each of its rows gets a run of
            // its own, still RUNNING; the two readers share the settled run.
            let (target, cancellable) = if operation == "CancelQuery" {
                let own = query_id(0x20 + (row * 4 + column) as u8);
                seed_query(&emulator, &own, STORE_ID, QueryState::Running);
                (own, true)
            } else {
                (stored_query.clone(), false)
            };
            let mut request = json!({"QueryId": target});
            if let Some(member) = &member {
                request["EventDataStore"] = json!(member);
            }
            let case = format!("{operation} on a stored query with {member_case}");
            let reply = emulator.call(operation, &request).await;
            if narrows_to_the_stored_store {
                let body = reply.expect_ok(&case);
                let expected_status = if cancellable {
                    QueryState::Cancelled.as_wire()
                } else {
                    QueryState::Finished.as_wire()
                };
                assert_eq!(
                    body.get("QueryStatus").and_then(Value::as_str),
                    Some(expected_status),
                    "case {case:?}: expected the run's status {expected_status:?}, got {body:?}"
                );
            } else {
                reply.expect_error(&case, NOT_FOUND_STATUS, "QueryIdNotFoundException");
            }
        }
    }

    // Only a value the service cannot read as a store at all is refused on the store: the
    // member's own form, before any query is resolved.
    for (case, identifier) in unreadable_identifiers() {
        for operation in ["DescribeQuery", "GetQueryResults", "CancelQuery"] {
            let case = format!("{operation} on {case}");
            emulator
                .call(
                    operation,
                    &json!({"QueryId": stored_query, "EventDataStore": identifier}),
                )
                .await
                .expect_error(&case, 400, "EventDataStoreARNInvalidException");
        }
    }

    // A READABLE ARN whose resource id no store carries is not a form error here: it
    // narrows the lookup to a store the stored query did not run on, so these operations
    // answer about the QUERY. (The federation and event-configuration operations judge
    // that id as well; see the suite's other test.)
    for operation in ["DescribeQuery", "GetQueryResults", "CancelQuery"] {
        let case = format!("{operation} narrowed to a store id no store carries");
        let reply = emulator
            .call(
                operation,
                &json!({
                    "QueryId": stored_query,
                    "EventDataStore": arn_with_a_resource_id_no_store_carries(),
                }),
            )
            .await;
        reply.expect_error(&case, NOT_FOUND_STATUS, "QueryIdNotFoundException");
    }
}
