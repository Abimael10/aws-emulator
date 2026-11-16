// SPDX-License-Identifier: CC0-1.0

//! Collection READS of this service: what they report when the account holds nothing, and
//! what they report once it holds something.
//!
//! RULE: a Glue read whose answer carries a collection reports that collection PRESENT —
//! empty when the account holds nothing under it and when a page lands past the end of the
//! listing — and never a missing member and never a refusal; a `@length` minimum on the
//! listed shape bounds what a non-empty collection may carry, not whether the member is
//! reported at all. What the collection holds is computed from the store, so a listed
//! record is the one the single-record read reports.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rows differ along what the rule generalises over: which operation is read, the
//! member its collection travels under, how many records the store holds, the page size
//! asked for (the published bounds included), which filter criteria narrow the listing, and
//! whether a batch's names resolve.
//!
//! The items member of each operation below comes from its own output shape
//! (`smithy_query(query="operation", operation="<Operation>")`), not from the wording of
//! any one answer.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::messages::INCLUDE_ROOT_NEEDS_THE_ACCOUNT_CATALOG;
use service_com_amazonaws_glue_awsglue::state::TERMINAL_DATA_QUALITY_RUN_STATES;
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok, ACCOUNT_ID, REGION};

/// The IAM role a data quality run names: "The IAM role that Glue assumes to access
/// resources for the run."
const ROLE: &str = "arn:aws:iam::000000000000:role/glue-data-quality-role";

/// How long a poll for a condition the service owes keeps looking before giving up. Far
/// larger than any span this emulator settles a run in: the loop waits for the CONDITION,
/// and this bound only stops it from running forever on a broken build.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The list a response reports under `member`, which must be PRESENT — an absent member is
/// the defect this suite is about, so it panics rather than reading absence as empty.
fn reported_list(case: &str, body: &Value, member: &str) -> Vec<Value> {
    body.get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the answer must report {member} as a list, got {body}")
        })
        .clone()
}

// ---------------------------------------------------------------------------
// An account that holds nothing
// ---------------------------------------------------------------------------

/// Every collection read of this service answers 200 on a fresh account with its items
/// member present and EMPTY, and with no continuation token, because nothing remains.
///
/// The rows are the collection reads this service serves, each with the member its
/// collection travels under and the smallest request that addresses it: the name listings,
/// the object listings, the run histories, the catalog listing and a batch reader whose
/// names resolve to nothing.
#[tokio::test]
async fn a_collection_read_of_an_empty_account_reports_its_items_member_empty() {
    let router = emulator();

    let cases: Vec<(&str, &str, &str, Value)> = vec![
        ("workflow names", "ListWorkflows", "Workflows", json!({})),
        ("usage profiles", "ListUsageProfiles", "Profiles", json!({})),
        (
            "data quality results",
            "ListDataQualityResults",
            "Results",
            json!({}),
        ),
        (
            "data quality rule recommendation runs",
            "ListDataQualityRuleRecommendationRuns",
            "Runs",
            json!({}),
        ),
        (
            "data quality ruleset evaluation runs",
            "ListDataQualityRulesetEvaluationRuns",
            "Runs",
            json!({}),
        ),
        ("catalogs", "GetCatalogs", "CatalogList", json!({})),
        // Rows the reported defect did not cite, which the same rule covers.
        ("trigger names", "ListTriggers", "TriggerNames", json!({})),
        ("job names", "ListJobs", "JobNames", json!({})),
        (
            "custom entity types",
            "ListCustomEntityTypes",
            "CustomEntityTypes",
            json!({}),
        ),
        ("blueprint names", "ListBlueprints", "Blueprints", json!({})),
        ("crawler names", "ListCrawlers", "CrawlerNames", json!({})),
        (
            "data quality rulesets",
            "ListDataQualityRulesets",
            "Rulesets",
            json!({}),
        ),
        (
            "a batch of blueprints none of which is stored",
            "BatchGetBlueprints",
            "Blueprints",
            json!({ "Names": ["absent-blueprint"] }),
        ),
    ];

    for (case, operation, member, request) in cases {
        let (status, body) = call(&router, operation, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must answer 200 on an empty account, got {status} with \
             {body}"
        );
        let listed = reported_list(case, &body, member);
        assert!(
            listed.is_empty(),
            "case {case:?}: an empty account must report an empty {member}, got {listed:?}"
        );
        assert!(
            body.get("NextToken").is_none_or(Value::is_null),
            "case {case:?}: nothing remains, so no continuation token may be reported, got {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// Usage profiles
// ---------------------------------------------------------------------------

/// Create one usage profile, and answer the body the create was sent.
async fn given_usage_profile(router: &MultiServiceRouter, name: &str, description: Option<&str>) {
    let mut body = json!({
        "Name": name,
        "Configuration": {
            "JobConfiguration": {
                "numberOfWorkers": { "DefaultValue": "5", "MinValue": "1", "MaxValue": "10" },
            },
        },
    });
    if let Some(description) = description {
        body["Description"] = json!(description);
    }
    ok(router, "CreateUsageProfile", &body).await;
}

/// The `Name` of every profile one page reports, in the order the service returned them.
fn listed_profile_names(case: &str, page: &Value) -> Vec<String> {
    reported_list(case, page, "Profiles")
        .iter()
        .map(|profile| {
            profile
                .get("Name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every listed profile carries a Name, got {profile}")
                })
                .to_string()
        })
        .collect()
}

/// `ListUsageProfiles` reports the profiles the store holds, in name order, one
/// `MaxResults`-sized page at a time, and the definition it reports for a profile is the one
/// the single-profile read reports.
///
/// The rows differ along the page size asked for: both published bounds of
/// `OrchestrationPageSize200` (1 and 200), a size inside it, and a size past the number of
/// stored profiles.
#[tokio::test]
async fn usage_profiles_are_listed_back_from_the_store_one_page_at_a_time() {
    let router = emulator();

    let stored = [
        ("analytics-profile", Some("the analytics team's limits")),
        ("bursty-profile", None),
        ("default-profile", Some("")),
        ("zeta-profile", Some("the last profile by name")),
    ];
    for (name, description) in stored {
        given_usage_profile(&router, name, description).await;
    }
    let expected: Vec<String> = {
        let mut names: Vec<String> = stored.iter().map(|(name, _)| (*name).to_string()).collect();
        names.sort();
        names
    };

    for (case, page_size) in [
        ("no page size", None),
        ("the smallest page the model admits", Some(1)),
        ("a page smaller than the listing", Some(2)),
        ("a page past the stored count", Some(10)),
        ("the largest page the model admits", Some(200)),
    ] {
        let mut listed: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut request = json!({});
            if let Some(size) = page_size {
                request["MaxResults"] = json!(size);
            }
            if let Some(cursor) = &token {
                request["NextToken"] = json!(cursor);
            }
            let page = ok(&router, "ListUsageProfiles", &request).await;
            let names = listed_profile_names(case, &page);
            if let Some(size) = page_size {
                assert!(
                    names.len() <= usize::try_from(size).unwrap_or(usize::MAX),
                    "case {case:?}: a page must not exceed MaxResults {size}, got {names:?}"
                );
            }
            listed.extend(names);
            token = page
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if token.is_none() {
                break;
            }
            assert!(
                listed.len() <= expected.len(),
                "case {case:?}: the walk must terminate, got {listed:?} for {expected:?}"
            );
        }
        assert_eq!(
            listed, expected,
            "case {case:?}: expected the stored profiles in name order {expected:?}, got {listed:?}"
        );
    }

    // What a listing reports for one profile is what the single-profile read reports.
    let page = ok(&router, "ListUsageProfiles", &json!({})).await;
    for profile in reported_list("profile projections", &page, "Profiles") {
        let name = profile
            .get("Name")
            .and_then(Value::as_str)
            .expect("every listed profile carries a Name")
            .to_string();
        let read = ok(&router, "GetUsageProfile", &json!({ "Name": name })).await;
        for member in ["Name", "Description", "CreatedOn", "LastModifiedOn"] {
            assert_eq!(
                profile.get(member),
                read.get(member),
                "case {name:?}: the listed {member} must be the one GetUsageProfile reports, got \
                 {profile} against {read}"
            );
        }
        assert!(
            profile.get("Configuration").is_none(),
            "case {name:?}: a UsageProfileDefinition carries no Configuration, got {profile}"
        );
    }
}

/// A page size outside `MaxResults`'s own range is refused rather than served: the member is
/// `OrchestrationPageSize200`, "range 1..200", so 0 and 201 are both outside it.
#[tokio::test]
async fn a_usage_profile_page_size_outside_the_published_range_is_refused() {
    let router = emulator();
    given_usage_profile(&router, "analytics-profile", None).await;

    for (case, size) in [("one below the minimum", 0), ("one past the maximum", 201)] {
        let (status, body) =
            call(&router, "ListUsageProfiles", &json!({ "MaxResults": size })).await;
        assert_ne!(
            status, 200,
            "case {case:?}: MaxResults {size} is outside the published range, so it must be \
             refused, got {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// Catalogs
// ---------------------------------------------------------------------------

/// A `FederatedCatalog` naming the source outside the Glue Data Catalog that the catalog
/// `name` points at: `CreateCatalog` registers only a catalog that names such a source.
fn federation_source(name: &str) -> Value {
    json!({
        "Identifier": format!("arn:aws:s3tables:{REGION}:{ACCOUNT_ID}:bucket/{name}"),
        "ConnectionName": "aws:s3tables",
    })
}

/// Register one catalog in the account's own Data Catalog.
async fn given_catalog(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateCatalog",
        &json!({
            "Name": name,
            "CatalogInput": { "FederatedCatalog": federation_source(name) },
        }),
    )
    .await;
}

/// The `Name` of every catalog one answer reports, in the order the service returned them.
fn listed_catalog_names(case: &str, page: &Value) -> Vec<String> {
    reported_list(case, page, "CatalogList")
        .iter()
        .map(|catalog| {
            catalog
                .get("Name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every listed catalog carries a Name, got {catalog}")
                })
                .to_string()
        })
        .collect()
}

/// `GetCatalogs` reports the catalogs the store holds inside the parent the request
/// addresses, one page at a time, with the account's own Data Catalog among them only where
/// the request asks for it.
///
/// The rows differ along which parent is addressed, whether the default catalog is asked
/// for, whether the walk is recursive, whether only catalogs that can hold databases are
/// wanted, and the page size.
#[tokio::test]
async fn catalogs_are_listed_back_from_the_parent_they_were_created_in() {
    let router = emulator();

    let stored = ["analytics", "finance", "marketing"];
    for name in stored {
        given_catalog(&router, name).await;
    }
    let created: Vec<String> = stored.iter().map(|name| (*name).to_string()).collect();
    let with_root: Vec<String> = {
        // The account's own Data Catalog answers to the account's number, which is the prefix
        // of every catalog id inside it, so it is reported ahead of them.
        let mut names = vec![ACCOUNT_ID.to_string()];
        names.extend(created.clone());
        names
    };

    let cases: Vec<(&str, Value, &Vec<String>)> = vec![
        ("no member at all", json!({}), &created),
        (
            "the account's own catalog as the parent",
            json!({ "ParentCatalogId": ACCOUNT_ID }),
            &created,
        ),
        (
            "the default catalog asked for",
            json!({ "IncludeRoot": true }),
            &with_root,
        ),
        (
            "the default catalog declined",
            json!({ "IncludeRoot": false }),
            &created,
        ),
        (
            "the whole hierarchy below the parent",
            json!({ "Recursive": true }),
            &created,
        ),
        (
            "only catalogs that can hold databases",
            json!({ "HasDatabases": true }),
            &created,
        ),
        (
            "the default catalog and the whole hierarchy",
            json!({ "IncludeRoot": true, "Recursive": true }),
            &with_root,
        ),
    ];

    for (case, request, expected) in cases {
        let page = ok(&router, "GetCatalogs", &request).await;
        assert_eq!(
            &listed_catalog_names(case, &page),
            expected,
            "case {case:?}: expected the catalogs {expected:?}, got {page}"
        );
    }

    // One catalog per page: the walk follows the service's own token to the end of the
    // listing and reports every catalog exactly once, in the same order.
    let mut listed: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({ "MaxResults": 1, "IncludeRoot": true });
        if let Some(cursor) = &token {
            request["NextToken"] = json!(cursor);
        }
        let page = ok(&router, "GetCatalogs", &request).await;
        let names = listed_catalog_names("one catalog per page", &page);
        assert!(
            names.len() <= 1,
            "case \"one catalog per page\": a page must not exceed MaxResults 1, got {names:?}"
        );
        listed.extend(names);
        token = page
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            break;
        }
        assert!(
            listed.len() <= with_root.len(),
            "case \"one catalog per page\": the walk must terminate, got {listed:?}"
        );
    }
    assert_eq!(
        listed, with_root,
        "case \"one catalog per page\": expected {with_root:?}, got {listed:?}"
    );

    // A catalog resource holds databases rather than catalogs, so what it holds in CATALOGS
    // is the empty listing — present, not absent, and not a refusal.
    let page = ok(
        &router,
        "GetCatalogs",
        &json!({ "ParentCatalogId": format!("{ACCOUNT_ID}:analytics") }),
    )
    .await;
    assert!(
        reported_list("a catalog as the parent", &page, "CatalogList").is_empty(),
        "case \"a catalog as the parent\": a catalog holds no catalog, so the listing is empty, \
         got {page}"
    );
}

/// `GetCatalogs` refuses a parent nothing was stored under, a parent in another account's
/// Data Catalog, and a request that asks about the default catalog while addressing a parent
/// that is not the account's own Data Catalog.
#[tokio::test]
async fn get_catalogs_refuses_a_parent_it_cannot_list() {
    let router = emulator();
    given_catalog(&router, "analytics").await;

    let cases: Vec<(&str, Value, &str, Option<&str>)> = vec![
        (
            "a name this account holds no catalog under",
            json!({ "ParentCatalogId": "no-such-catalog" }),
            "EntityNotFoundException",
            None,
        ),
        (
            "a catalog in another account's Data Catalog",
            json!({ "ParentCatalogId": "210987654321:analytics" }),
            "AccessDeniedException",
            None,
        ),
        (
            "the default catalog asked about under a catalog parent",
            json!({ "ParentCatalogId": format!("{ACCOUNT_ID}:analytics"), "IncludeRoot": true }),
            "InvalidInputException",
            Some(INCLUDE_ROOT_NEEDS_THE_ACCOUNT_CATALOG),
        ),
        (
            "the default catalog declined under a catalog parent",
            json!({ "ParentCatalogId": format!("{ACCOUNT_ID}:analytics"), "IncludeRoot": false }),
            "InvalidInputException",
            Some(INCLUDE_ROOT_NEEDS_THE_ACCOUNT_CATALOG),
        ),
    ];

    for (case, request, code, message) in cases {
        let (status, body) = call(&router, "GetCatalogs", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the refusal's status 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with(code),
            "case {case:?}: expected {code}, got {}",
            error_code(&body)
        );
        if let Some(message) = message {
            assert_eq!(
                error_message(&body),
                message,
                "case {case:?}: expected the refusal's wording, got {body}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Blueprints, read in a batch
// ---------------------------------------------------------------------------

/// Register one blueprint.
async fn given_blueprint(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateBlueprint",
        &json!({
            "Name": name,
            "BlueprintLocation": format!("s3://glue-blueprints/{name}.zip"),
        }),
    )
    .await;
}

/// One row of the blueprint batch table: the names a batch asks for, the names it must
/// report back as registrations, and the names it must report as missing.
struct BatchCase<'a> {
    /// What this row is, named in every assertion it makes.
    case: &'a str,
    /// The `Names` the batch asks for, in the request's order.
    names: Vec<&'a str>,
    /// The names that must come back under `Blueprints`, in order.
    found: Vec<&'a str>,
    /// The names that must come back under `MissingBlueprints`, in order.
    missing: Vec<&'a str>,
}

/// `BatchGetBlueprints` answers one record per REQUESTED name: the registration this account
/// holds under it, or the name itself under `MissingBlueprints`.
///
/// The rows differ along how many names are asked for, whether each resolves, the order they
/// are asked in and whether the batch repeats one.
#[tokio::test]
async fn a_blueprint_batch_reports_what_the_store_holds_under_each_name() {
    let router = emulator();
    for name in ["bronze-ingest", "silver-curate"] {
        given_blueprint(&router, name).await;
    }

    let cases = vec![
        BatchCase {
            case: "one stored name",
            names: vec!["bronze-ingest"],
            found: vec!["bronze-ingest"],
            missing: vec![],
        },
        BatchCase {
            case: "every stored name",
            names: vec!["silver-curate", "bronze-ingest"],
            found: vec!["silver-curate", "bronze-ingest"],
            missing: vec![],
        },
        BatchCase {
            case: "a stored name and a name nothing was registered under",
            names: vec!["bronze-ingest", "gold-publish"],
            found: vec!["bronze-ingest"],
            missing: vec!["gold-publish"],
        },
        BatchCase {
            case: "nothing but names nothing was registered under",
            names: vec!["gold-publish", "platinum-publish"],
            found: vec![],
            missing: vec!["gold-publish", "platinum-publish"],
        },
        BatchCase {
            case: "a name asked for twice",
            names: vec!["bronze-ingest", "bronze-ingest"],
            found: vec!["bronze-ingest", "bronze-ingest"],
            missing: vec![],
        },
        BatchCase {
            case: "a missing name asked for twice",
            names: vec!["gold-publish", "gold-publish"],
            found: vec![],
            missing: vec!["gold-publish", "gold-publish"],
        },
    ];

    for row in cases {
        let BatchCase {
            case,
            names,
            found,
            missing,
        } = row;
        let body = ok(
            &router,
            "BatchGetBlueprints",
            &json!({ "Names": names.clone() }),
        )
        .await;
        let listed: Vec<String> = reported_list(case, &body, "Blueprints")
            .iter()
            .map(|blueprint| {
                blueprint
                    .get("Name")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: every blueprint carries a Name, got {blueprint}")
                    })
                    .to_string()
            })
            .collect();
        assert_eq!(
            listed, found,
            "case {case:?}: expected the blueprints {found:?} in the request's order {names:?}, \
             got {listed:?}"
        );
        // `MissingBlueprints` targets `BlueprintNames`, which the model bounds `length 1..25`,
        // so an empty list is not a value of that shape: a batch that missed nothing reports
        // no such member at all, which is the convention this crate's other batch reader
        // follows for its own missing-names member (`tests/all/workflow_batch_reads.rs`
        // pins it for `BatchGetWorkflows$MissingWorkflows`). The found list is a different
        // member of a different shape and is always present, empty included.
        if missing.is_empty() {
            assert!(
                body.get("MissingBlueprints").is_none_or(Value::is_null),
                "case {case:?}: a batch that missed nothing reports no MissingBlueprints, got \
                 {body}"
            );
            continue;
        }
        let reported_missing = crate::wire::string_list(&body, "MissingBlueprints");
        assert_eq!(
            reported_missing, missing,
            "case {case:?}: expected the missing names {missing:?}, got {reported_missing:?}"
        );
    }

    // What a batch reports for one blueprint is what the single-blueprint read reports.
    let body = ok(
        &router,
        "BatchGetBlueprints",
        &json!({ "Names": ["bronze-ingest"] }),
    )
    .await;
    let batched = reported_list("batch against single read", &body, "Blueprints")
        .first()
        .cloned()
        .expect("the batch reports the stored blueprint");
    let read = ok(
        &router,
        "GetBlueprint",
        &json!({ "Name": "bronze-ingest" }),
    )
    .await;
    let single = read
        .get("Blueprint")
        .unwrap_or_else(|| panic!("GetBlueprint must report a Blueprint, got {read}"));
    for member in ["Name", "BlueprintLocation", "CreatedOn", "Version"] {
        assert_eq!(
            batched.get(member),
            single.get(member),
            "case \"batch against single read\": the batched {member} must be the one GetBlueprint \
             reports, got {batched} against {single}"
        );
    }
}

// ---------------------------------------------------------------------------
// Data quality run histories
// ---------------------------------------------------------------------------

/// Store `database` holding `table`, the catalog state a data quality run is resolved
/// against.
async fn given_table(router: &MultiServiceRouter, database: &str, table: &str) {
    let (status, body) = call(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": database } }),
    )
    .await;
    assert!(
        status == 200 || error_code(&body).ends_with("AlreadyExistsException"),
        "CreateDatabase must answer 200 or report the database is already stored, got {status} \
         with {body}"
    );
    ok(
        router,
        "CreateTable",
        &json!({
            "DatabaseName": database,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": format!("s3://bucket/{table}/"),
                    "Columns": [{ "Name": "id", "Type": "string" }],
                },
            },
        }),
    )
    .await;
}

/// A `DataSource` addressing `database`.`table`.
fn glue_table_source(database: &str, table: &str) -> Value {
    json!({ "GlueTable": { "DatabaseName": database, "TableName": table } })
}

/// Start a rule recommendation run over `database`.`table` and answer its `RunId`.
async fn given_recommendation_run(
    router: &MultiServiceRouter,
    database: &str,
    table: &str,
) -> String {
    let started = ok(
        router,
        "StartDataQualityRuleRecommendationRun",
        &json!({ "DataSource": glue_table_source(database, table), "Role": ROLE }),
    )
    .await;
    started
        .get("RunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a started run must report a RunId, got {started}"))
        .to_string()
}

/// The runs `ListDataQualityRuleRecommendationRuns` reports for `request`.
async fn listed_runs(router: &MultiServiceRouter, case: &str, request: &Value) -> Vec<Value> {
    let page = ok(router, "ListDataQualityRuleRecommendationRuns", request).await;
    reported_list(case, &page, "Runs")
}

/// `ListDataQualityRuleRecommendationRuns` reports the runs the store holds, narrowed by
/// every criterion the filter names, and the description it reports for a run is the one the
/// single-run read reports.
///
/// The rows differ along which criteria narrow the listing: none, the data source of each
/// covered table, an instant window that admits the runs and one that admits none, and a tag
/// filter — and the refusal row along a data source that addresses no table at all.
#[tokio::test]
async fn data_quality_recommendation_runs_are_listed_back_from_the_store() {
    let router = emulator();
    given_table(&router, "sales", "orders").await;
    given_table(&router, "sales", "returns").await;
    let over_orders = given_recommendation_run(&router, "sales", "orders").await;
    let over_returns = given_recommendation_run(&router, "sales", "returns").await;

    // The statuses the clock owes these runs are waited for by LOOKING AGAIN, so what the
    // rows below compare is two reads of settled runs rather than a race.
    let deadline = SystemTime::now() + POLL_DEADLINE;
    loop {
        let runs = listed_runs(&router, "settled runs", &json!({})).await;
        let settled = runs.iter().all(|run| {
            run.get("Status")
                .and_then(Value::as_str)
                .is_some_and(|status| TERMINAL_DATA_QUALITY_RUN_STATES.contains(&status))
        });
        if settled && runs.len() == 2 {
            break;
        }
        assert!(
            SystemTime::now() < deadline,
            "the clock owes both runs a terminal status; the listing still reports {runs:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let future = json!(4_102_444_800i64);

    let cases: Vec<(&str, Value, Vec<&String>)> = vec![
        (
            "no criteria at all",
            json!({}),
            vec![&over_returns, &over_orders],
        ),
        (
            "the data source of one covered table",
            json!({ "Filter": { "DataSource": glue_table_source("sales", "orders") } }),
            vec![&over_orders],
        ),
        (
            "the data source of the other covered table",
            json!({ "Filter": { "DataSource": glue_table_source("sales", "returns") } }),
            vec![&over_returns],
        ),
        (
            "another casing of a covered table",
            json!({ "Filter": { "DataSource": glue_table_source("SALES", "ORDERS") } }),
            vec![&over_orders],
        ),
        (
            "a table no run covers",
            json!({ "Filter": { "DataSource": glue_table_source("sales", "shipments") } }),
            vec![],
        ),
        (
            "runs started after an instant long past",
            json!({
                "Filter": {
                    "DataSource": glue_table_source("sales", "orders"),
                    "StartedAfter": 0,
                },
            }),
            vec![&over_orders],
        ),
        (
            // An instant long ahead of any run this suite started, as an awsJson timestamp:
            // epoch seconds, the wire form every timestamp of this protocol travels in.
            "runs started after an instant still to come",
            json!({
                "Filter": {
                    "DataSource": glue_table_source("sales", "orders"),
                    "StartedAfter": future,
                },
            }),
            vec![],
        ),
        (
            "a tag no run carries",
            json!({ "Tags": { "team": "analytics" } }),
            vec![],
        ),
    ];

    for (case, request, expected) in cases {
        let runs = listed_runs(&router, case, &request).await;
        let ids: Vec<String> = runs
            .iter()
            .map(|run| {
                run.get("RunId")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("case {case:?}: every run carries a RunId, got {run}"))
                    .to_string()
            })
            .collect();
        let expected: Vec<String> = expected.into_iter().cloned().collect();
        assert_eq!(
            ids, expected,
            "case {case:?}: expected the runs {expected:?}, newest first, got {ids:?}"
        );
    }

    // What a listing reports for one run is what the single-run read reports.
    for run in listed_runs(&router, "run projections", &json!({})).await {
        let id = run
            .get("RunId")
            .and_then(Value::as_str)
            .expect("every run carries a RunId")
            .to_string();
        let read = ok(
            &router,
            "GetDataQualityRuleRecommendationRun",
            &json!({ "RunId": id }),
        )
        .await;
        for member in [
            "RunId",
            "Status",
            "StartedOn",
            "DataSource",
            "CreatedRulesetName",
            "RecommendationMode",
        ] {
            assert_eq!(
                run.get(member),
                read.get(member),
                "case {id:?}: the listed {member} must be the one the single-run read reports, got \
                 {run} against {read}"
            );
        }
    }
}

/// Both data quality listings refuse a filter whose `DataSource` addresses no table at all,
/// and serve one that addresses a table no record covers as the EMPTY listing.
#[tokio::test]
async fn a_data_quality_filter_without_a_table_is_refused_and_one_matching_nothing_is_empty() {
    let router = emulator();

    let cases: Vec<(&str, &str, &str)> = vec![
        (
            "recommendation runs",
            "ListDataQualityRuleRecommendationRuns",
            "Runs",
        ),
        ("results", "ListDataQualityResults", "Results"),
    ];

    for (case, operation, member) in cases {
        let (status, body) = call(
            &router,
            operation,
            &json!({ "Filter": { "DataSource": {} } }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a DataSource addressing no table must be refused, got {status} with \
             {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case:?}: expected InvalidInputException, got {}",
            error_code(&body)
        );

        let page = ok(
            &router,
            operation,
            &json!({ "Filter": { "DataSource": glue_table_source("sales", "orders") } }),
        )
        .await;
        assert!(
            reported_list(case, &page, member).is_empty(),
            "case {case:?}: a filter matching no record must report an empty {member}, got {page}"
        );
    }

    // Criteria the store can carry no record for narrow the listing to nothing rather than
    // widening it: a result this service holds was published by a ruleset evaluation run, so
    // none carries a job name or a job run id.
    for (case, filter) in [
        ("a job name", json!({ "JobName": "nightly-etl" })),
        ("a job run id", json!({ "JobRunId": "jr_0123456789" })),
    ] {
        let page = ok(
            &router,
            "ListDataQualityResults",
            &json!({ "Filter": filter }),
        )
        .await;
        assert!(
            reported_list(case, &page, "Results").is_empty(),
            "case {case:?}: no stored result carries one, so the listing is empty, got {page}"
        );
    }
}

// ---------------------------------------------------------------------------
// Data quality results published by a finished evaluation run
// ---------------------------------------------------------------------------

/// The bucket the staged table of the results test points its data at.
const RESULT_DATA_BUCKET: &str = "dq-listing-data";

/// The rows the staged table holds: enough for a rule over the row count to be evaluated at
/// all, which is what makes the run publish a result.
const RESULT_ROWS: &str = "1,ACTIVE\n2,INACTIVE\n";

/// An emulator with Glue AND Amazon S3 mounted on one shared engine: a ruleset is evaluated
/// against the objects behind its data source's table, so the data has to be reachable
/// through the same engine.
fn emulator_with_s3() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = crate::wire::emulator_on(std::sync::Arc::clone(&engine));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// Store `database` holding `table`, with [`RESULT_ROWS`] staged behind it as the delimited
/// object an evaluation run reads.
async fn given_csv_table(router: &MultiServiceRouter, database: &str, table: &str) {
    let (status, body) = call(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": database } }),
    )
    .await;
    assert!(
        status == 200 || error_code(&body).ends_with("AlreadyExistsException"),
        "CreateDatabase must answer 200 or report the database is already stored, got {status} \
         with {body}"
    );
    let client = crate::wire::s3_client(router);
    // The bucket is shared by the tables of this test, so a create of an existing one is no
    // failure.
    let _ = client.create_bucket().bucket(RESULT_DATA_BUCKET).send().await;
    client
        .put_object()
        .bucket(RESULT_DATA_BUCKET)
        .key(format!("{table}/rows.csv"))
        .body(aws_sdk_s3::primitives::ByteStream::from(
            RESULT_ROWS.as_bytes().to_vec(),
        ))
        .send()
        .await
        .unwrap_or_else(|error| panic!("the data of {table} must be staged, got {error:?}"));
    ok(
        router,
        "CreateTable",
        &json!({
            "DatabaseName": database,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": format!("s3://{RESULT_DATA_BUCKET}/{table}/"),
                    "Columns": [
                        { "Name": "id", "Type": "string" },
                        { "Name": "status", "Type": "string" },
                    ],
                    "InputFormat": "org.apache.hadoop.mapred.TextInputFormat",
                    "SerdeInfo": {
                        "SerializationLibrary": "org.apache.hadoop.hive.serde2.OpenCSVSerde",
                        "Parameters": { "separatorChar": "," },
                    },
                },
            },
        }),
    )
    .await;
}

/// `ListDataQualityResults` reports the results a finished evaluation run published, newest
/// first, narrowed by every criterion the filter names, and the description it reports for a
/// result is the one `GetDataQualityResult` reports.
///
/// The rows differ along which criteria narrow the listing: none, the data source of each
/// evaluated table, a table no result covers, an instant window that admits the results and
/// one that admits none, and the page size — one result per page against a page larger than
/// the listing.
#[tokio::test]
async fn published_data_quality_results_are_listed_back_from_the_store() {
    let router = emulator_with_s3();
    given_csv_table(&router, "sales", "orders").await;
    given_csv_table(&router, "sales", "returns").await;
    ok(
        &router,
        "CreateDataQualityRuleset",
        &json!({ "Name": "row-count", "Ruleset": "Rules = [\n    RowCount > 0\n]" }),
    )
    .await;
    for table in ["orders", "returns"] {
        ok(
            &router,
            "StartDataQualityRulesetEvaluationRun",
            &json!({
                "DataSource": glue_table_source("sales", table),
                "Role": ROLE,
                "RulesetNames": ["row-count"],
            }),
        )
        .await;
    }

    // One result per run falls due on the clock, so the listing is polled until both runs have
    // published theirs rather than waited on for a fixed span.
    let deadline = SystemTime::now() + POLL_DEADLINE;
    let published = loop {
        let page = ok(&router, "ListDataQualityResults", &json!({})).await;
        let results = reported_list("published results", &page, "Results");
        if results.len() == 2 {
            break results;
        }
        assert!(
            SystemTime::now() < deadline,
            "the clock owes each run its result; the listing still reports {results:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    // Newest first: each listed result starts no later than the one ahead of it.
    let instants: Vec<f64> = published
        .iter()
        .map(|result| {
            result
                .get("StartedOn")
                .and_then(Value::as_f64)
                .unwrap_or_else(|| panic!("every listed result carries a StartedOn, got {result}"))
        })
        .collect();
    assert!(
        instants.windows(2).all(|pair| pair[0] >= pair[1]),
        "case \"newest first\": expected the results in non-increasing StartedOn order, got \
         {instants:?}"
    );

    let listed_ids = |page: &Value, case: &str| -> Vec<String> {
        reported_list(case, page, "Results")
            .iter()
            .map(|result| {
                result
                    .get("ResultId")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("case {case:?}: every result carries a ResultId, got {result}"))
                    .to_string()
            })
            .collect()
    };
    let every_id = listed_ids(&ok(&router, "ListDataQualityResults", &json!({})).await, "all");

    for (case, request, expected) in [
        ("no criteria at all", json!({}), 2_usize),
        (
            "the data source of one evaluated table",
            json!({ "Filter": { "DataSource": glue_table_source("sales", "orders") } }),
            1,
        ),
        (
            "the data source of the other evaluated table",
            json!({ "Filter": { "DataSource": glue_table_source("sales", "returns") } }),
            1,
        ),
        (
            "another casing of an evaluated table",
            json!({ "Filter": { "DataSource": glue_table_source("SALES", "ORDERS") } }),
            1,
        ),
        (
            "a table no result covers",
            json!({ "Filter": { "DataSource": glue_table_source("sales", "shipments") } }),
            0,
        ),
        (
            // An instant long past, as an awsJson timestamp: epoch seconds.
            "results started after an instant long past",
            json!({ "Filter": { "StartedAfter": 0 } }),
            2,
        ),
        (
            "results started before an instant long past",
            json!({ "Filter": { "StartedBefore": 0 } }),
            0,
        ),
        (
            "a page larger than the listing",
            json!({ "MaxResults": 1_000 }),
            2,
        ),
    ] {
        let page = ok(&router, "ListDataQualityResults", &request).await;
        let ids = listed_ids(&page, case);
        assert_eq!(
            ids.len(),
            expected,
            "case {case:?}: expected {expected} result(s), got {ids:?}"
        );
        assert!(
            ids.iter().all(|id| every_id.contains(id)),
            "case {case:?}: every reported result must be one the store published {every_id:?}, \
             got {ids:?}"
        );
    }

    // One result per page: the walk follows the service's own token to the end of the listing
    // and reports each result exactly once.
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({ "MaxResults": 1 });
        if let Some(cursor) = &token {
            request["NextToken"] = json!(cursor);
        }
        let page = ok(&router, "ListDataQualityResults", &request).await;
        let ids = listed_ids(&page, "one result per page");
        assert!(
            ids.len() <= 1,
            "case \"one result per page\": a page must not exceed MaxResults 1, got {ids:?}"
        );
        walked.extend(ids);
        token = page
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            break;
        }
        assert!(
            walked.len() <= every_id.len(),
            "case \"one result per page\": the walk must terminate, got {walked:?}"
        );
    }
    assert_eq!(
        walked, every_id,
        "case \"one result per page\": expected the same results in the same order as one page, \
         got {walked:?}"
    );

    // What a listing reports for one result is what the single-result read reports.
    for result in &published {
        let id = result
            .get("ResultId")
            .and_then(Value::as_str)
            .expect("every listed result carries a ResultId")
            .to_string();
        let read = ok(
            &router,
            "GetDataQualityResult",
            &json!({ "ResultId": id }),
        )
        .await;
        for member in ["ResultId", "DataSource", "StartedOn"] {
            assert_eq!(
                result.get(member),
                read.get(member),
                "case {id:?}: the listed {member} must be the one GetDataQualityResult reports, \
                 got {result} against {read}"
            );
        }
        for member in ["JobName", "JobRunId"] {
            assert!(
                result.get(member).is_none_or(Value::is_null),
                "case {id:?}: a result published by an evaluation run carries no {member}, got \
                 {result}"
            );
        }
    }
}
