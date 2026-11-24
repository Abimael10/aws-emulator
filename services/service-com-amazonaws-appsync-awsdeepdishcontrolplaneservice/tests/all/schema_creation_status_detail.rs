// SPDX-License-Identifier: CC0-1.0

//! The DETAIL `GetSchemaCreationStatus` reports beside the status, over every state that
//! status can be in.
//!
//! The rule asserted here is stated over the whole CLASS of schema states rather than over
//! the one request that exposed the defect: the status and "Detailed information about the
//! status of the schema creation operation" are the two members of one report, so every
//! status this call reports comes with the non-empty detail of that state — the state an
//! API no creation has ever run for is in included, which carries the detail of having had
//! no creation performed and not a missing member.
//!
//! The table's rows differ along what the rule generalises over: whether a creation ran at
//! all, the `apiType` of the API read, how many types the submitted document declares,
//! whether the document defines a type system the API can hold, whether the payload is
//! text at all, and how many times the status is read. Between them the rows reach every
//! `SchemaStatus` this service can report, and the test asserts that each of those states
//! was in fact reached and detailed.
//!
//! Beside the table, two further properties of the same rule: the detail of the state no
//! creation applies to interpolates nothing of the API that reports it (two APIs of
//! different `apiType` report the identical sentence), and it is left behind for good once
//! a creation has run — the detail FOLLOWS the state rather than being one sentence for
//! everything.
//!
//! The harness is the schema suites' own — the router the production emulator mounts,
//! `create_api`, `start_creation`/`creation_status`, the poll that waits for the creation
//! the clock owes and the `SchemaStatus` catalog — so every assertion goes through the
//! routing, the (de)serialization and the handlers the emulator serves, and a schema
//! request is built exactly one way across this crate.

use std::collections::BTreeMap;

use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;

use crate::schema_creation::{
    await_settled_creation, creation_status, start_creation, SCHEMA_STATUSES,
};
use crate::source_api_association::{create_api, router, Answer};

/// One row of the detail table.
#[derive(Debug, Clone)]
struct Row {
    label: &'static str,
    /// The `apiType` the API is created with: a schema creation is a property of the API,
    /// whichever kind of API holds it.
    api_type: &'static str,
    /// The document the creation carries, or `None` for an API no creation runs for.
    definition: Option<&'static [u8]>,
    /// How many further reads are taken, after the first one they are all held to.
    rereads: usize,
    /// The status every read owes once the clock has performed whatever was owed.
    settled: &'static str,
}

/// The cases the rule is asserted over: no creation at all on either kind of API that
/// holds one, documents declaring one type and several, a document GraphQL cannot parse,
/// and a payload that is not text — each read a different number of times.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "no creation on a GraphQL API",
            api_type: state::GRAPHQL_API_TYPE,
            definition: None,
            rereads: 2,
            settled: state::SCHEMA_NOT_APPLICABLE,
        },
        Row {
            label: "no creation on a Merged API",
            api_type: state::MERGED_API_TYPE,
            definition: None,
            rereads: 1,
            settled: state::SCHEMA_NOT_APPLICABLE,
        },
        Row {
            label: "a document declaring one type",
            api_type: state::GRAPHQL_API_TYPE,
            definition: Some(b"type Query {\n  hello: String\n}\n"),
            rereads: 1,
            settled: state::SCHEMA_SUCCESS,
        },
        Row {
            label: "a document declaring several types",
            api_type: state::GRAPHQL_API_TYPE,
            definition: Some(
                b"schema { query: Query }\ntype Query {\n  post: Post\n}\n\ntype Post {\n  id: ID!\n}\n",
            ),
            rereads: 3,
            settled: state::SCHEMA_SUCCESS,
        },
        Row {
            label: "a document GraphQL cannot parse",
            api_type: state::GRAPHQL_API_TYPE,
            definition: Some(b"type Query {\n  hello: String\n"),
            rereads: 2,
            settled: state::SCHEMA_FAILED,
        },
        Row {
            label: "a payload that is not text",
            api_type: state::GRAPHQL_API_TYPE,
            definition: Some(&[0xff, 0xfe, 0x00, 0x01]),
            rereads: 1,
            settled: state::SCHEMA_FAILED,
        },
    ]
}

/// The status and the detail one read reported, as the rule reads them: a 200 whose status
/// is a value the model declares and whose detail is there and not empty.
fn reported(answer: &Answer, case: &str) -> (String, String) {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected GetSchemaCreationStatus 200, got {} {:?}",
        answer.status, answer.body
    );
    let status = answer.body["status"].as_str().unwrap_or_default();
    assert!(
        SCHEMA_STATUSES.contains(&status),
        "case {case:?}: expected a modelled SchemaStatus out of {SCHEMA_STATUSES:?}, got \
         {status:?}"
    );
    let detail = answer.body["details"].as_str();
    assert!(
        detail.is_some_and(|text| !text.is_empty()),
        "case {case:?}: expected the {status} status to be reported with a non-empty detail, got \
         {:?}",
        answer.body
    );
    (
        status.to_string(),
        detail.unwrap_or_default().to_string(),
    )
}

#[tokio::test]
async fn every_reported_schema_status_comes_with_the_detail_of_that_state() {
    let router = router();
    let rows = rows();
    let mut apis: Vec<String> = Vec::new();
    // What each state was detailed as, so the rows together are held to covering every
    // status this service reports.
    let mut detailed: BTreeMap<String, String> = BTreeMap::new();

    // Every row's creation is scheduled first, each on its own API, so the ONE wait
    // further down is crossed by all of them: the transition is owed by the clock.
    for row in &rows {
        let api = create_api(&router, row.label, row.api_type).await;
        if let Some(definition) = row.definition {
            let started = start_creation(&router, &api.id, definition).await;
            assert_eq!(
                started.status, 200,
                "case {:?}: expected StartSchemaCreation 200, got {} {:?}",
                row.label, started.status, started.body
            );
        }

        // The FIRST read is what every later one is held to: the detail of the state the
        // API is in, which is the scheduled creation's where one is owed and the state no
        // creation applies to where none ever ran.
        let first = creation_status(&router, &api.id).await;
        let (status, detail) = reported(&first, row.label);
        let owed = if row.definition.is_some() {
            (state::SCHEMA_PROCESSING, state::SCHEMA_PROCESSING_DETAIL)
        } else {
            (
                state::SCHEMA_NOT_APPLICABLE,
                state::SCHEMA_NOT_APPLICABLE_DETAIL,
            )
        };
        assert_eq!(
            (status.as_str(), detail.as_str()),
            owed,
            "case {:?}: expected the state before the clock settles anything to be reported as \
             {owed:?}, got {:?}",
            row.label,
            first.body
        );
        detailed.insert(status, detail);

        for reread in 1..=row.rereads {
            let again = creation_status(&router, &api.id).await;
            assert_eq!(
                again.body, first.body,
                "case {:?} reread {reread}: expected reading the status to leave the same report \
                 for the next caller as {:?}, got {:?}",
                row.label, first.body, again.body
            );
        }

        apis.push(api.id);
    }

    for (row, api_id) in rows.iter().zip(&apis) {
        // Nothing is submitted in between: the status below is the one the clock owed.
        let settled = if row.definition.is_some() {
            await_settled_creation(&router, api_id, row.label).await
        } else {
            creation_status(&router, api_id).await
        };
        let (status, detail) = reported(&settled, row.label);
        assert_eq!(
            status, row.settled,
            "case {:?}: expected the settled state to be reported as {:?}, got {:?}",
            row.label, row.settled, settled.body
        );

        // The detail is the one that goes with THIS state: a creation that ran details its
        // own outcome, so neither the scheduled creation's sentence nor the one for a state
        // no creation applies to is still being reported.
        if row.definition.is_some() {
            assert!(
                detail != state::SCHEMA_PROCESSING_DETAIL
                    && detail != state::SCHEMA_NOT_APPLICABLE_DETAIL,
                "case {:?}: expected the performed creation to detail its own outcome, got \
                 {detail:?}",
                row.label
            );
        } else {
            assert_eq!(
                detail,
                state::SCHEMA_NOT_APPLICABLE_DETAIL,
                "case {:?}: expected the detail of the state no creation applies to, got {:?}",
                row.label,
                settled.body
            );
        }
        detailed.insert(status, detail);

        for reread in 1..=row.rereads {
            let again = creation_status(&router, api_id).await;
            assert_eq!(
                again.body, settled.body,
                "case {:?} reread {reread} past the settled state: expected the report to be \
                 unchanged by reading it, got {:?}",
                row.label, again.body
            );
        }
    }

    // The rows between them reached every status this service reports, and each one was
    // reported with a detail of its own: no two states share a sentence.
    let reached: Vec<&String> = detailed.keys().collect();
    let expected = [
        state::SCHEMA_NOT_APPLICABLE,
        state::SCHEMA_PROCESSING,
        state::SCHEMA_SUCCESS,
        state::SCHEMA_FAILED,
    ];
    for status in expected {
        assert!(
            detailed.contains_key(status),
            "case the whole table: expected {status:?} to have been reported and detailed, got \
             {reached:?}"
        );
    }
    let mut sentences: Vec<&String> = detailed.values().collect();
    sentences.sort();
    let distinct = sentences.len();
    sentences.dedup();
    assert_eq!(
        sentences.len(),
        distinct,
        "case the whole table: expected each state's detail to be its own, got {detailed:?}"
    );
}

#[tokio::test]
async fn the_detail_of_a_state_no_creation_applies_to_names_no_api_and_is_left_behind() {
    let router = router();

    // It reports the state, not the API: two APIs that differ in name and in `apiType`
    // report the identical sentence, so nothing of the API is interpolated into it.
    let graphql = create_api(&router, "detail-no-creation-graphql", state::GRAPHQL_API_TYPE).await;
    let merged = create_api(&router, "detail-no-creation-merged", state::MERGED_API_TYPE).await;
    for (case, api_id) in [("a GraphQL API", &graphql.id), ("a Merged API", &merged.id)] {
        let answer = creation_status(&router, api_id).await;
        let (status, detail) = reported(&answer, case);
        assert_eq!(
            (status.as_str(), detail.as_str()),
            (
                state::SCHEMA_NOT_APPLICABLE,
                state::SCHEMA_NOT_APPLICABLE_DETAIL
            ),
            "case {case:?}: expected the state no creation applies to and its detail, got {:?}",
            answer.body
        );
        assert!(
            !detail.contains(api_id),
            "case {case:?}: expected a detail that names no API, got {detail:?}"
        );
    }

    // Once a creation has run, that detail is gone for good: the sentence follows the
    // state, so every later read details the creation instead.
    let started = start_creation(
        &router,
        &graphql.id,
        b"schema { query: Query }\ntype Query {\n  hello: String\n}\n",
    )
    .await;
    assert_eq!(
        started.status, 200,
        "case the creation that leaves the state behind: expected 200, got {} {:?}",
        started.status, started.body
    );
    let scheduled = creation_status(&router, &graphql.id).await;
    let (status, detail) = reported(&scheduled, "the scheduled creation");
    assert_eq!(
        (status.as_str(), detail.as_str()),
        (state::SCHEMA_PROCESSING, state::SCHEMA_PROCESSING_DETAIL),
        "case the scheduled creation: expected the non-terminal state and its detail, got {:?}",
        scheduled.body
    );

    let performed = await_settled_creation(&router, &graphql.id, "the performed creation").await;
    let (status, detail) = reported(&performed, "the performed creation");
    assert_eq!(
        status,
        state::SCHEMA_SUCCESS,
        "case the performed creation: expected {:?}, got {:?}",
        state::SCHEMA_SUCCESS,
        performed.body
    );
    assert_ne!(
        detail,
        state::SCHEMA_NOT_APPLICABLE_DETAIL,
        "case the performed creation: expected the creation's own detail, got the one of the state \
         no creation applies to"
    );

    // The API that had no creation is untouched by the one that ran next door: its own
    // state, and its own detail, are still what they were.
    let untouched = creation_status(&router, &merged.id).await;
    let (status, detail) = reported(&untouched, "the API beside it");
    assert_eq!(
        (status.as_str(), detail.as_str()),
        (
            state::SCHEMA_NOT_APPLICABLE,
            state::SCHEMA_NOT_APPLICABLE_DETAIL
        ),
        "case the API beside it: expected its own state and detail, got {:?}",
        untouched.body
    );
}
