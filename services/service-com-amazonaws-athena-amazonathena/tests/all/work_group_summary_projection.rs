// SPDX-License-Identifier: CC0-1.0

//! What a `WorkGroupSummary` carries, and how a paginated read of this service refuses a
//! `NextToken` it did not mint — both driven end to end over the same router the emulator
//! boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`), so every assertion is
//! about what an SDK client observes.
//!
//! THE RULES UNDER TEST:
//!
//! 1. A summary reports every member `WorkGroupSummary` carries for the workgroup it
//!    names: the description — the EMPTY string for a workgroup nobody described, since
//!    `WorkGroupDescriptionString` is `@length 0..1024` — and the engine version with the
//!    engine FAMILY beside the selected and effective versions, the family following from
//!    the version the workgroup's queries actually run on.
//! 2. A pagination token this service did not mint names no page, and the refusal says so
//!    in Athena's own wording: the fixed text `The nextPageToken is malformed: ` followed
//!    by the token exactly as the request carried it, the same for every paginated read of
//!    this service.
//!
//! Each rule is asserted over a TABLE: the rows of the first differ along what the rule
//! generalises over — whether a create ever registered the workgroup, whether it carried a
//! description (absent, empty, written, and the longest the model admits) and which engine
//! version it selected (none, the SQL line, and the Spark one, i.e. both families); the
//! rows of the second along the operation refusing the token and the form of the token
//! offered.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    page_token_malformed, DEFAULT_EFFECTIVE_ENGINE_VERSION, DEFAULT_WORK_GROUP,
    ENGINE_CATEGORY_PRESTO, ENGINE_CATEGORY_SPARK, ENGINE_VERSION_AUTO, ERROR_CODE_INVALID_INPUT,
    PYSPARK_ENGINE_VERSION,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, message, router};

/// The longest description `WorkGroupDescriptionString` admits (`@length 0..1024`).
const LONGEST_DESCRIPTION: usize = 1024;

/// The engine family Athena publishes for one engine version, written out here rather
/// than derived by the code under test, so a wrong mapping fails rather than agrees with
/// itself.
///
/// The versions are the ones `ListEngineVersions` publishes as selectable — Athena's SQL
/// line and the Auto option that resolves into it, and Athena's Spark line — and the two
/// families are the ones Athena runs them on. A version this table does not name is a
/// version whose family nobody wrote down, which the caller must add here.
fn published_family(version: &str) -> &'static str {
    match version {
        "AUTO" | "Athena engine version 2" | "Athena engine version 3" => ENGINE_CATEGORY_PRESTO,
        "PySpark engine version 3" | "Apache Spark version 3.5" => ENGINE_CATEGORY_SPARK,
        other => panic!("case {other:?}: name this published version's engine family here"),
    }
}

/// One `ListWorkGroups` call that must be accepted, answering the summaries it carried.
async fn summaries(router: &MultiServiceRouter) -> Vec<Value> {
    let request = json!({ "MaxResults": 50 });
    let (status, listed) = call(router, "ListWorkGroups", &request).await;
    assert_eq!(
        status, 200,
        "ListWorkGroups({request}) must answer 200, got {status} with {listed:?}"
    );
    listed["WorkGroups"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("a listing must carry WorkGroups, got {listed:?}"))
}

/// The summary of `name` in that listing.
fn summary_of(listed: &[Value], case: &str, name: &str) -> Value {
    listed
        .iter()
        .find(|summary| summary["Name"].as_str() == Some(name))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: {name} must be listed, got {listed:?}"))
}

/// One row of the summary table.
struct Row {
    /// What this row varies.
    case: &'static str,
    /// The workgroup's name.
    name: String,
    /// The `Description` the create carried, or `None` for a create that carried none —
    /// and for the account's default workgroup, which no create registered at all.
    description: Option<String>,
    /// The `SelectedEngineVersion` the create carried, if any.
    selected: Option<&'static str>,
    /// Whether a `CreateWorkGroup` call registers this row, or it is the workgroup the
    /// account already holds.
    created: bool,
}

/// The rows of that table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the account's default workgroup, which no create registered",
            name: DEFAULT_WORK_GROUP.to_string(),
            description: None,
            selected: None,
            created: false,
        },
        Row {
            case: "a create that carried no description",
            name: "summary-no-description".to_string(),
            description: None,
            selected: None,
            created: true,
        },
        Row {
            case: "a create that carried the empty description",
            name: "summary-empty-description".to_string(),
            description: Some(String::new()),
            selected: None,
            created: true,
        },
        Row {
            case: "a create that carried a description",
            name: "summary-described".to_string(),
            description: Some("where the nightly rollups run".to_string()),
            selected: None,
            created: true,
        },
        Row {
            case: "a description of the longest modelled length",
            name: "summary-longest-description".to_string(),
            description: Some("d".repeat(LONGEST_DESCRIPTION)),
            selected: None,
            created: true,
        },
        Row {
            case: "a create that pinned an Athena SQL engine version",
            name: "summary-sql-pinned".to_string(),
            description: None,
            selected: Some(DEFAULT_EFFECTIVE_ENGINE_VERSION),
            created: true,
        },
        Row {
            case: "a create that selected the Spark engine version",
            name: "summary-spark".to_string(),
            description: Some("notebook sessions live here".to_string()),
            selected: Some(PYSPARK_ENGINE_VERSION),
            created: true,
        },
    ]
}

/// Every summary a listing reports carries the workgroup's description — empty rather than
/// absent when nobody wrote one — and its engine version with the family beside the
/// versions.
#[tokio::test]
async fn every_work_group_summary_reports_its_description_and_engine_family() {
    let router = router();
    let rows = rows();
    for row in rows.iter().filter(|row| row.created) {
        let mut request = json!({ "Name": row.name });
        if let Some(description) = row.description.as_deref() {
            request["Description"] = json!(description);
        }
        if let Some(selected) = row.selected {
            request["Configuration"] =
                json!({ "EngineVersion": { "SelectedEngineVersion": selected } });
        }
        let (status, created) = call(&router, "CreateWorkGroup", &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateWorkGroup must answer 200, got {status} with {created:?}",
            row.case
        );
    }

    let listed = summaries(&router).await;
    for row in &rows {
        let summary = summary_of(&listed, row.case, &row.name);

        // A workgroup nobody described has the empty description, which is a value the
        // member's 0-character minimum admits — not a member the client is not told about.
        let expected_description = row.description.clone().unwrap_or_default();
        assert_eq!(
            summary["Description"].as_str(),
            Some(expected_description.as_str()),
            "case {:?}: expected the description {expected_description:?}, got {summary:?}",
            row.case
        );

        let expected_selected = row.selected.unwrap_or(ENGINE_VERSION_AUTO);
        let expected_effective = row.selected.unwrap_or(DEFAULT_EFFECTIVE_ENGINE_VERSION);
        let version = &summary["EngineVersion"];
        assert_eq!(
            version["SelectedEngineVersion"].as_str(),
            Some(expected_selected),
            "case {:?}: expected the selected version {expected_selected:?}, got {summary:?}",
            row.case
        );
        assert_eq!(
            version["EffectiveEngineVersion"].as_str(),
            Some(expected_effective),
            "case {:?}: expected the effective version {expected_effective:?}, got {summary:?}",
            row.case
        );
        // The family follows from the version the queries run on: Athena names its Spark
        // versions after Spark and its SQL versions after its own engine line.
        let expected_category = published_family(expected_effective);
        assert_eq!(
            version["Category"].as_str(),
            Some(expected_category),
            "case {:?}: expected the engine family {expected_category:?}, got {summary:?}",
            row.case
        );
    }
}

/// The family an engine version published as selectable belongs to is the family of that
/// same version, whichever listing reports it — so a client that classifies a workgroup by
/// its family classifies the catalog's entries the same way.
#[tokio::test]
async fn every_published_engine_version_reports_its_family() {
    let router = router();
    let (status, listed) = call(&router, "ListEngineVersions", &json!({})).await;
    assert_eq!(
        status, 200,
        "ListEngineVersions must answer 200, got {status} with {listed:?}"
    );
    let entries = listed["EngineVersions"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("a listing must carry EngineVersions, got {listed:?}"));
    assert!(
        !entries.is_empty(),
        "the catalog of selectable versions must not be empty, got {listed:?}"
    );
    for entry in &entries {
        let selected = entry["SelectedEngineVersion"]
            .as_str()
            .unwrap_or_else(|| panic!("every entry names a selectable version, got {entry:?}"));
        let expected = published_family(selected);
        assert_eq!(
            entry["Category"].as_str(),
            Some(expected),
            "case {selected:?}: expected the engine family {expected:?}, got {entry:?}"
        );
    }
}

/// A token this service did not mint for the listing it was offered to names no page, and
/// every paginated read refuses it in the same sentence, naming the token it was given.
#[tokio::test]
async fn a_pagination_token_no_listing_minted_is_refused_naming_that_token() {
    let router = router();
    // Enough workgroups that a page of one leaves some behind, so this listing mints a
    // token of its own to offer to the OTHER listings.
    for index in 0..3 {
        let name = format!("token-row-{index}");
        let (status, created) = call(&router, "CreateWorkGroup", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "the workgroup {name} must be created, got {status} with {created:?}"
        );
    }
    let (status, first_page) = call(&router, "ListWorkGroups", &json!({ "MaxResults": 1 })).await;
    assert_eq!(
        status, 200,
        "a first page must answer 200, got {status} with {first_page:?}"
    );
    let work_groups_token = first_page["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a page of one of four workgroups hands a token back, got {first_page:?}"))
        .to_string();
    // A token of a SECOND listing, so the one offered to a given operation is always one
    // that operation itself did not mint: its own token names a page and is accepted.
    let (status, versions_page) =
        call(&router, "ListEngineVersions", &json!({ "MaxResults": 1 })).await;
    assert_eq!(
        status, 200,
        "a first page of the engine-version catalog must answer 200, got {status} with \
         {versions_page:?}"
    );
    let engine_versions_token = versions_page["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a page of one of several versions hands a token back, got {versions_page:?}"))
        .to_string();

    // (the case, the token offered) — a value that is no token at all, one short enough to
    // carry no authenticator, and one of the right shape that nobody computed.
    let tokens: Vec<(&str, String)> = vec![
        ("a value that is not a token at all", "notavalidnexttoken".to_string()),
        ("a value too short to carry an authenticator", "00".to_string()),
        (
            "hexadecimal of the right shape that nobody computed",
            ::hex::encode([0_u8; 48]),
        ),
    ];

    // Every paginated read whose token names a position in a listing of this account.
    for operation in [
        "ListWorkGroups",
        "ListDataCatalogs",
        "ListEngineVersions",
        "ListCapacityReservations",
    ] {
        // Beside the forms above, a token this service DID mint — for another of its
        // listings, which names no page of this one.
        let minted_elsewhere = if operation == "ListWorkGroups" {
            engine_versions_token.clone()
        } else {
            work_groups_token.clone()
        };
        let rows = tokens.iter().cloned().chain([(
            "a token this service minted for another of its listings",
            minted_elsewhere,
        )]);
        for (form, token) in rows {
            let case = format!("{operation} offered {form}");
            let (status, refused) = call(
                &router,
                operation,
                &json!({ "MaxResults": 2, "NextToken": token }),
            )
            .await;
            // Asserted against the wording itself, not only against the helper the
            // handlers word it with, so a change to that wording fails here rather than
            // agreeing with itself: the fixed text is Athena's and the token the CALLER
            // supplied is its one variable part.
            assert_refusal(
                &case,
                status,
                &refused,
                ERROR_CODE_INVALID_INPUT,
                &format!("The nextPageToken is malformed: {token}"),
            );
            assert_eq!(
                message(&refused),
                page_token_malformed(&token),
                "case {case:?}: expected the wording this service states a token refusal \
                 in, got {refused:?}"
            );
        }
    }

    // The refusals left the listing itself alone: it still reports every workgroup the
    // account holds, the default one included.
    let listed = summaries(&router).await;
    assert_eq!(
        listed.len(),
        4,
        "expected the three created workgroups and the default one, got {listed:?}"
    );
}
