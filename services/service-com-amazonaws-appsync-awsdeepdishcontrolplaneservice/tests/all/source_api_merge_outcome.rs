// SPDX-License-Identifier: CC0-1.0

//! What a source-API merge LEAVES BEHIND, end to end over HTTP: the terminal status it
//! settles into, the detail that identifies it, and the types the Merged API then holds.
//!
//! Three rules are asserted here, over a TABLE spanning the class each generalises over
//! rather than the one request that exposed the defect:
//!
//! 1. an association's merge is asynchronous but TERMINAL: whichever side of the pair
//!    created it (`AssociateMergedGraphqlApi` addresses the source API,
//!    `AssociateSourceGraphqlApi` the Merged API) and whatever the source API's own
//!    schema still owed when it was created, the association leaves the non-terminal
//!    `MERGE_SCHEDULED` for a terminal `SourceApiAssociationStatus` with no call but the
//!    reads that poll it (`smithy_query(query="shape",
//!    shape="SourceApiAssociationStatus")` — the terminal values are `MERGE_SUCCESS`,
//!    `MERGE_FAILED` and `AUTO_MERGE_SCHEDULE_FAILED`);
//! 2. on `MERGE_SUCCESS` the `sourceApiAssociationStatusDetail` states WHICH source API
//!    was merged and under WHICH association, both read from the association's own state
//!    — so every row's detail differs and no constant sentence satisfies them all;
//! 3. a merge that succeeded is one the Merged API's schema HOLDS: every type the source
//!    API declares is readable on the merged API id afterwards, carrying the source's own
//!    definition, and a merge whose source API still owed a schema creation when the
//!    association was made waits for that creation rather than merging an empty schema.
//!
//! The harness (`router`, `call`, `create_api`, `get_path`) is the one the association
//! suites already carry, so every assertion goes through the router the production
//! emulator mounts: routing, restJson1 (de)serialization, the handlers and the shared
//! engine together.

use serde_json::json;
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::schema_creation::{await_creation, get_type, start_creation};
use crate::source_api_association::{
    await_terminal_merge, call, create_api, router, TERMINAL_MERGE_STATUSES,
};

/// The schema document the source API of a row that has one is created with: one type
/// per root operation plus a type of its own, so the merged side has several names to
/// hold and one field to report.
const SOURCE_SCHEMA: &str = "type Query {\n  orders: Order\n}\ntype Order {\n  id: ID!\n}\n";

/// The types `SOURCE_SCHEMA` declares, and a field each definition must still carry once
/// it is read off the MERGED API.
const SOURCE_TYPES: &[(&str, &str)] = &[("Query", "orders"), ("Order", "id")];

/// Which side of the pair the row's request addresses.
#[derive(Debug, Clone, Copy)]
enum Side {
    /// `AssociateMergedGraphqlApi`: the path names the source API, the body the Merged
    /// API.
    FromTheSourceApi,
    /// `AssociateSourceGraphqlApi`: the path names the Merged API, the body the source
    /// API.
    FromTheMergedApi,
}

/// When the source API's schema is created, relative to the association.
#[derive(Debug, Clone, Copy)]
enum Schema {
    /// The creation is let come due before the association is made.
    CreatedFirst,
    /// The association is made while the creation the clock owes is still in flight, so
    /// the merge depends on it.
    StillInFlight,
    /// The source API holds no schema at all, so the merge carries no type.
    None,
}

/// One row: which side associated the pair, and what the source API's schema was doing.
#[derive(Debug)]
struct Case {
    label: &'static str,
    side: Side,
    schema: Schema,
}

/// The table: both association paths, and every state the source API's own schema can be
/// in when the association is created.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "from the source api, schema created first",
            side: Side::FromTheSourceApi,
            schema: Schema::CreatedFirst,
        },
        Case {
            label: "from the source api, schema still in flight",
            side: Side::FromTheSourceApi,
            schema: Schema::StillInFlight,
        },
        Case {
            label: "from the source api, no schema",
            side: Side::FromTheSourceApi,
            schema: Schema::None,
        },
        Case {
            label: "from the merged api, schema created first",
            side: Side::FromTheMergedApi,
            schema: Schema::CreatedFirst,
        },
        Case {
            label: "from the merged api, schema still in flight",
            side: Side::FromTheMergedApi,
            schema: Schema::StillInFlight,
        },
        Case {
            label: "from the merged api, no schema",
            side: Side::FromTheMergedApi,
            schema: Schema::None,
        },
    ]
}

/// The role a Merged API is created with, as the recorded setup creates one: "the IAM
/// role to be used by AppSync to merge the source APIs" (`GraphqlApi$mergedApiExecutionRoleArn`).
/// The ARN is this account's own, built the way every emulated ARN is, so no recorded
/// value is written into the fixture.
fn merge_execution_role() -> String {
    format!(
        "arn:aws:iam::{}:role/appsync-merge-execution",
        crate::source_api_association::ACCOUNT
    )
}

/// Create one Merged API with a merge execution role, which is how a Merged API that
/// merges source APIs is created.
async fn create_merged_api(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let answer = call(
        router,
        "POST",
        "/v1/apis",
        Some(json!({
            "name": name,
            "apiType": state::MERGED_API_TYPE,
            "authenticationType": "API_KEY",
            "mergedApiExecutionRoleArn": merge_execution_role(),
            "queryDepthLimit": 0,
            "resolverCountLimit": 0,
            "xrayEnabled": false,
        })),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case create the merged API {name:?}: expected 200, got {} {:?}",
        answer.status, answer.body
    );
    (
        answer.body["graphqlApi"]["apiId"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        answer.body["graphqlApi"]["mergedApiExecutionRoleArn"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// Create the row's pair, its source schema and its association, and answer the source
/// API id, the Merged API id and the `associationId`.
async fn fixture(router: &MultiServiceRouter, case: &Case) -> (String, String, String) {
    let source = create_api(
        router,
        &format!("{}-source", case.label),
        state::GRAPHQL_API_TYPE,
    )
    .await;
    let (merged_api_id, role) = create_merged_api(router, &format!("{}-merged", case.label)).await;
    assert_eq!(
        role,
        merge_execution_role(),
        "case {:?}: expected the merge execution role back from the create, got {role:?}",
        case.label
    );

    match case.schema {
        Schema::CreatedFirst | Schema::StillInFlight => {
            let started = start_creation(router, &source.id, SOURCE_SCHEMA.as_bytes()).await;
            assert_eq!(
                started.status, 200,
                "case {:?}: expected StartSchemaCreation 200, got {} {:?}",
                case.label, started.status, started.body
            );
            if matches!(case.schema, Schema::CreatedFirst) {
                await_creation(router, &source.id, case.label).await;
            }
        }
        Schema::None => {}
    }

    let (path, body) = match case.side {
        Side::FromTheSourceApi => (
            format!("/v1/sourceApis/{}/mergedApiAssociations", source.id),
            json!({"mergedApiIdentifier": merged_api_id}),
        ),
        Side::FromTheMergedApi => (
            format!("/v1/mergedApis/{merged_api_id}/sourceApiAssociations"),
            json!({"sourceApiIdentifier": source.id}),
        ),
    };
    let created = call(router, "POST", &path, Some(body)).await;
    assert_eq!(
        created.status, 200,
        "case {:?}: expected the association to be created, got {} {:?}",
        case.label, created.status, created.body
    );
    let association = &created.body["sourceApiAssociation"];
    assert_eq!(
        association["sourceApiAssociationStatus"].as_str(),
        Some(state::MERGE_SCHEDULED),
        "case {:?}: expected the create to report the non-terminal scheduled status, got {:?}",
        case.label,
        association
    );
    let association_id = association["associationId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (source.id, merged_api_id, association_id)
}

#[tokio::test]
async fn a_merge_settles_terminally_and_details_the_source_it_covered() {
    let router = router();
    let mut details: Vec<String> = Vec::new();

    for case in cases() {
        let (source_api_id, merged_api_id, association_id) = fixture(&router, &case).await;

        let settled =
            await_terminal_merge(&router, case.label, &merged_api_id, &association_id).await;
        let status = settled["sourceApiAssociationStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            TERMINAL_MERGE_STATUSES.contains(&status.as_str()),
            "case {:?}: expected one of the terminal statuses {TERMINAL_MERGE_STATUSES:?}, got \
             {status:?}",
            case.label
        );
        if matches!(case.schema, Schema::None) {
            // A source API with no schema at all is the row where the terminal status
            // this service settles into is not pinned: which of the modelled terminal
            // values AppSync reports for a source API that has nothing to merge is not
            // evidence this fix carries, so the row asserts only that the association
            // reaches one of them and leaves the detail and the date to the rows whose
            // merge did carry a schema.
            continue;
        }
        assert_eq!(
            status,
            state::MERGE_SUCCESS,
            "case {:?}: expected the merge of a schema this account holds to succeed, got {:?}",
            case.label,
            settled
        );
        assert!(
            settled["lastSuccessfulMergeDate"]
                .as_f64()
                .is_some_and(|epoch| epoch > 0.0),
            "case {:?}: expected the merge date the performed merge left, got {:?}",
            case.label,
            settled["lastSuccessfulMergeDate"]
        );

        // The detail identifies the merge: the source API and the association it
        // covered, both read from the association's own state. Written out here rather
        // than read off the service's own template, so a change of wording is caught
        // rather than followed.
        let detail = settled["sourceApiAssociationStatusDetail"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            detail,
            format!(
                "Successfully merged source GraphQL API (API Id: {source_api_id}, Association \
                 Id: {association_id})"
            ),
            "case {:?}: expected the detail to name the merged source API and association, got \
             {detail:?}",
            case.label
        );
        assert!(
            !details.contains(&detail),
            "case {:?}: expected a detail of this row's own merge, got {detail:?} which an \
             earlier row already reported",
            case.label
        );
        details.push(detail);
    }
}

#[tokio::test]
async fn a_merged_api_holds_the_types_of_the_source_it_merged() {
    let router = router();

    for case in cases() {
        let (source_api_id, merged_api_id, association_id) = fixture(&router, &case).await;
        await_terminal_merge(&router, case.label, &merged_api_id, &association_id).await;

        for (type_name, field) in SOURCE_TYPES {
            let on_the_source = get_type(&router, &source_api_id, type_name, state::SDL_FORMAT).await;
            let on_the_merged = get_type(&router, &merged_api_id, type_name, state::SDL_FORMAT).await;
            match case.schema {
                Schema::None => {
                    // The source API declared no type, so the merge carried none: the
                    // Merged API holds nothing under that name either.
                    assert_eq!(
                        (on_the_source.status, on_the_merged.status),
                        (404, 404),
                        "case {:?}, type {type_name:?}: expected no type on either side of a \
                         merge with no schema, got {on_the_source:?} and {on_the_merged:?}",
                        case.label
                    );
                }
                Schema::CreatedFirst | Schema::StillInFlight => {
                    assert_eq!(
                        on_the_merged.status, 200,
                        "case {:?}, type {type_name:?}: expected the merged API to hold the \
                         source API's type, got {} {:?}",
                        case.label, on_the_merged.status, on_the_merged.body
                    );
                    // The merged type IS the source's: same name, same definition, and
                    // the field the source declared is in it.
                    assert_eq!(
                        on_the_merged.body["type"]["definition"],
                        on_the_source.body["type"]["definition"],
                        "case {:?}, type {type_name:?}: expected the source API's own \
                         definition, got {:?} against {:?}",
                        case.label,
                        on_the_merged.body["type"],
                        on_the_source.body["type"]
                    );
                    let definition = on_the_merged.body["type"]["definition"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    assert!(
                        definition.contains(field),
                        "case {:?}, type {type_name:?}: expected the merged definition to carry \
                         the field {field:?}, got {definition:?}",
                        case.label
                    );
                    // The type is reported under the MERGED API's own path, which is
                    // where it now lives.
                    let arn = on_the_merged.body["type"]["arn"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    assert!(
                        arn.contains(&merged_api_id),
                        "case {:?}, type {type_name:?}: expected an ARN under the merged API \
                         {merged_api_id:?}, got {arn:?}",
                        case.label
                    );
                }
            }
        }
    }
}
