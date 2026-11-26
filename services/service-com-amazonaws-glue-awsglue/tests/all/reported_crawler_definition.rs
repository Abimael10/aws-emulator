// SPDX-License-Identifier: CC0-1.0

//! What a crawler READ reports beyond the members the write named: the sub-structures of the
//! definition the service materialises, and the form the crawler's execution role is
//! reported in.
//!
//! RULE: a read of a crawler reports every member of the crawler definition the service
//! holds a value for — the policies and configurations it defaults (`SchemaChangePolicy`,
//! `RecrawlPolicy`, `LineageConfiguration`, `LakeFormationConfiguration`), each member of a
//! sub-structure the write left unset at the service's own default, every target list the
//! write named no target in as the EMPTY list (`Exclusions` on an Amazon S3 target
//! included), and the table-profiling setting the crawler is in — whether or not the write
//! that stored it mentioned any of them. The crawler's `Role`, which a write may name "the
//! IAM role or Amazon Resource Name (ARN) of an IAM role" by (`CreateCrawler$Role`), is
//! reported as the NAME IAM holds that role under.
//!
//! The rule is about the crawler a read reports, so it holds on every surface that reports
//! one: the single read, the listing and the batch read all answer the same crawler.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rows differ along what the rule generalises over: which of the four defaulted
//! sub-structures the write states and how much of each (neither member, one member, both),
//! which target list the write fills and whether its Amazon S3 target states exclusions, and
//! the form the write names the role in (a bare name, an ARN at the account root, an ARN
//! under a path, and an ARN of another account, which names no role of this account to take
//! a name out of).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{emulator, ok, ACCOUNT_ID};

/// The database the crawlers of this suite write their tables into.
const DATABASE: &str = "reported_crawler_db";

/// Every target list a crawler reports: the eight the published `CrawlerTargets` binds
/// (`smithy_query shape=CrawlerTargets`) and the REST catalog list the service reports
/// beside them.
const TARGET_LISTS: [&str; 9] = [
    "S3Targets",
    "JdbcTargets",
    "MongoDBTargets",
    "DynamoDBTargets",
    "CatalogTargets",
    "DeltaTargets",
    "IcebergTargets",
    "HudiTargets",
    "RestCatalogTargets",
];

/// The four sub-structures of a crawler definition the service DEFAULTS, each at the value a
/// crawler whose write stated none of it is in force with.
///
/// Sources: `SchemaChangePolicy` (`UpdateBehavior` of the modelled `UpdateBehavior` enum that
/// writes the changed schema into the catalog, `DeleteBehavior` of `DeleteBehavior` that
/// deprecates a table whose data is gone), `RecrawlPolicy$RecrawlBehavior` ("a value of
/// `CRAWL_EVERYTHING` specifies crawling the entire dataset again"),
/// `LineageConfiguration$CrawlerLineageSettings` (data lineage off) and
/// `LakeFormationConfiguration` ("for same account crawls as the target data, this can be
/// left as null", with Lake Formation credentials not used).
fn defaulted_substructures() -> Value {
    json!({
        "SchemaChangePolicy": {
            "UpdateBehavior": "UPDATE_IN_DATABASE",
            "DeleteBehavior": "DEPRECATE_IN_DATABASE",
        },
        "RecrawlPolicy": { "RecrawlBehavior": "CRAWL_EVERYTHING" },
        "LineageConfiguration": { "CrawlerLineageSettings": "DISABLE" },
        "LakeFormationConfiguration": {
            "UseLakeFormationCredentials": false,
            "AccountId": "",
        },
    })
}

/// One row: the crawler the write stores, and what the read must then report for it.
struct Row {
    /// What every assertion of the row reports.
    case: &'static str,
    /// The members the `CreateCrawler` request carries beside `Name` and `DatabaseName`.
    sent: Value,
    /// The `Role` the read must report: the name IAM holds the written role under.
    role: &'static str,
    /// The members of the reported crawler the row pins beyond the role, merged over
    /// [`defaulted_substructures`] — what the write stated of a defaulted sub-structure, and
    /// the target lists it filled.
    reported: Value,
}

/// The rows: one per shape of write the rule covers.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a crawler whose write stated nothing but its role and one S3 target",
            sent: json!({
                "Role": "GlueCrawlerRole",
                "Targets": { "S3Targets": [{ "Path": "s3://reported/one/" }] },
            }),
            role: "GlueCrawlerRole",
            reported: json!({
                "S3Targets": [{ "Path": "s3://reported/one/", "Exclusions": [] }],
            }),
        },
        Row {
            case: "a role named by its ARN at the account root",
            sent: json!({
                "Role": "arn:aws:iam::000000000000:role/GlueCrawlerRole",
                "Targets": { "S3Targets": [{ "Path": "s3://reported/arn/" }] },
            }),
            role: "GlueCrawlerRole",
            reported: json!({
                "S3Targets": [{ "Path": "s3://reported/arn/", "Exclusions": [] }],
            }),
        },
        Row {
            case: "a role named by an ARN under a path",
            sent: json!({
                "Role": "arn:aws:iam::000000000000:role/service-role/AWSGlueServiceRole-v2",
                "Targets": { "S3Targets": [{ "Path": "s3://reported/path/" }] },
            }),
            role: "AWSGlueServiceRole-v2",
            reported: json!({
                "S3Targets": [{ "Path": "s3://reported/path/", "Exclusions": [] }],
            }),
        },
        Row {
            case: "a role named by an ARN of another account, which names no role here",
            sent: json!({
                "Role": "arn:aws:iam::210987654321:role/CrossAccountCrawlerRole",
                "Targets": { "S3Targets": [{ "Path": "s3://reported/foreign/" }] },
            }),
            // Nothing of this account's IAM is named, so there is no name inside the value
            // for the service to report: it answers the value as it was given.
            role: "arn:aws:iam::210987654321:role/CrossAccountCrawlerRole",
            reported: json!({
                "S3Targets": [{ "Path": "s3://reported/foreign/", "Exclusions": [] }],
            }),
        },
        Row {
            case: "an S3 target that states its own exclusions",
            sent: json!({
                "Role": "GlueCrawlerRole",
                "Targets": {
                    "S3Targets": [{
                        "Path": "s3://reported/excluded/",
                        "Exclusions": ["**/_SUCCESS", "**/*.tmp"],
                    }],
                },
            }),
            role: "GlueCrawlerRole",
            reported: json!({
                "S3Targets": [{
                    "Path": "s3://reported/excluded/",
                    "Exclusions": ["**/_SUCCESS", "**/*.tmp"],
                }],
            }),
        },
        Row {
            case: "a write that states ONE member of each policy, leaving the other to the \
                   service",
            sent: json!({
                "Role": "GlueCrawlerRole",
                "Targets": { "JdbcTargets": [{ "ConnectionName": "warehouse" }] },
                "SchemaChangePolicy": { "UpdateBehavior": "LOG" },
                "RecrawlPolicy": { "RecrawlBehavior": "CRAWL_NEW_FOLDERS_ONLY" },
            }),
            role: "GlueCrawlerRole",
            reported: json!({
                "JdbcTargets": [{ "ConnectionName": "warehouse" }],
                "SchemaChangePolicy": {
                    "UpdateBehavior": "LOG",
                    "DeleteBehavior": "DEPRECATE_IN_DATABASE",
                },
                "RecrawlPolicy": { "RecrawlBehavior": "CRAWL_NEW_FOLDERS_ONLY" },
            }),
        },
        Row {
            case: "a write that states every defaulted sub-structure in full",
            sent: json!({
                "Role": "GlueCrawlerRole",
                "Targets": { "DynamoDBTargets": [{ "Path": "orders" }] },
                "SchemaChangePolicy": {
                    "UpdateBehavior": "LOG",
                    "DeleteBehavior": "DELETE_FROM_DATABASE",
                },
                "RecrawlPolicy": { "RecrawlBehavior": "CRAWL_EVENT_MODE" },
                "LineageConfiguration": { "CrawlerLineageSettings": "ENABLE" },
                "LakeFormationConfiguration": {
                    "UseLakeFormationCredentials": true,
                    "AccountId": ACCOUNT_ID,
                },
            }),
            role: "GlueCrawlerRole",
            reported: json!({
                "DynamoDBTargets": [{ "Path": "orders" }],
                "SchemaChangePolicy": {
                    "UpdateBehavior": "LOG",
                    "DeleteBehavior": "DELETE_FROM_DATABASE",
                },
                "RecrawlPolicy": { "RecrawlBehavior": "CRAWL_EVENT_MODE" },
                "LineageConfiguration": { "CrawlerLineageSettings": "ENABLE" },
                "LakeFormationConfiguration": {
                    "UseLakeFormationCredentials": true,
                    "AccountId": ACCOUNT_ID,
                },
            }),
        },
        Row {
            case: "a Data Catalog target, which fills a list no other row fills",
            sent: json!({
                "Role": "GlueCrawlerRole",
                "Targets": {
                    "CatalogTargets": [{
                        "DatabaseName": DATABASE,
                        "Tables": ["orders"],
                    }],
                },
            }),
            role: "GlueCrawlerRole",
            reported: json!({
                "CatalogTargets": [{ "DatabaseName": DATABASE, "Tables": ["orders"] }],
            }),
        },
        Row {
            case: "an Iceberg target, whose own members the write states",
            sent: json!({
                "Role": "GlueCrawlerRole",
                "Targets": {
                    "IcebergTargets": [{
                        "Paths": ["s3://reported/iceberg/"],
                        "MaximumTraversalDepth": 3,
                    }],
                },
            }),
            role: "GlueCrawlerRole",
            reported: json!({
                "IcebergTargets": [{
                    "Paths": ["s3://reported/iceberg/"],
                    "MaximumTraversalDepth": 3,
                }],
            }),
        },
    ]
}

/// The whole `Targets` structure the read must report for a row: every list it filled, and
/// every other list EMPTY.
fn expected_targets(reported: &Value) -> Value {
    let mut targets = json!({});
    for list in TARGET_LISTS {
        targets[list] = reported.get(list).cloned().unwrap_or_else(|| json!([]));
    }
    targets
}

/// The four defaulted sub-structures as the row's write leaves them: the service's default
/// for each member the write did not state.
fn expected_substructures(reported: &Value) -> Value {
    let mut expected = defaulted_substructures();
    let members = expected.as_object_mut().expect("a structure");
    for (member, stated) in reported.as_object().into_iter().flatten() {
        if let Some(defaulted) = members.get_mut(member) {
            // A write that states one member of a sub-structure leaves the others at the
            // service's default, so the stated members are merged over them.
            for (inner, value) in stated.as_object().into_iter().flatten() {
                defaulted[inner] = value.clone();
            }
        }
    }
    expected
}

/// Create the row's crawler under `name`.
async fn create(router: &MultiServiceRouter, name: &str, row: &Row) {
    let mut request = row.sent.clone();
    request["Name"] = json!(name);
    request["DatabaseName"] = json!(DATABASE);
    ok(router, "CreateCrawler", &request).await;
}

/// Assert that `crawler` is the row's crawler as the rule says a read reports it.
fn assert_reported(case: &str, surface: &str, crawler: &Value, row: &Row) {
    assert_eq!(
        crawler.get("Role").and_then(Value::as_str),
        Some(row.role),
        "case {case:?} / {surface}: expected the role reported as {:?}, got {crawler}",
        row.role
    );
    let targets = expected_targets(&row.reported);
    assert_eq!(
        crawler["Targets"], targets,
        "case {case:?} / {surface}: expected the targets {targets}, got {crawler}"
    );
    for (member, expected) in expected_substructures(&row.reported)
        .as_object()
        .into_iter()
        .flatten()
    {
        assert_eq!(
            crawler.get(member),
            Some(expected),
            "case {case:?} / {surface}: expected {member} {expected}, got {crawler}"
        );
    }
    assert_eq!(
        crawler.get("TableProfilingConfiguration"),
        Some(&json!({ "CollectTableStatistics": false })),
        "case {case:?} / {surface}: expected the table-profiling setting the crawler is in, \
         got {crawler}"
    );
}

/// Every surface that reports a crawler reports the one the rule describes: the single read,
/// the listing and the batch read.
#[tokio::test]
async fn every_crawler_read_reports_the_definition_the_service_holds() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    let rows = rows();
    let mut names: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let name = format!("reported-crawler-{index}");
        create(&router, &name, row).await;
        names.push(name);
    }

    // The single read.
    for (name, row) in names.iter().zip(rows.iter()) {
        let read = ok(&router, "GetCrawler", &json!({ "Name": name })).await;
        assert_reported(row.case, "GetCrawler", &read["Crawler"], row);
    }

    // The listing: one page over every crawler the account holds.
    let listed = ok(&router, "GetCrawlers", &json!({})).await;
    let crawlers = listed["Crawlers"]
        .as_array()
        .unwrap_or_else(|| panic!("GetCrawlers must report a list, got {listed}"));
    assert_eq!(
        crawlers.len(),
        rows.len(),
        "the listing must report every crawler the table created, got {listed}"
    );
    for (name, row) in names.iter().zip(rows.iter()) {
        let crawler = crawlers
            .iter()
            .find(|crawler| crawler["Name"] == json!(name))
            .unwrap_or_else(|| panic!("case {:?}: {name} must be listed, got {listed}", row.case));
        assert_reported(row.case, "GetCrawlers", crawler, row);
    }

    // The batch read, which names every crawler at once.
    let batch = ok(
        &router,
        "BatchGetCrawlers",
        &json!({ "CrawlerNames": names }),
    )
    .await;
    let crawlers = batch["Crawlers"]
        .as_array()
        .unwrap_or_else(|| panic!("BatchGetCrawlers must report a list, got {batch}"));
    for (name, row) in names.iter().zip(rows.iter()) {
        let crawler = crawlers
            .iter()
            .find(|crawler| crawler["Name"] == json!(name))
            .unwrap_or_else(|| panic!("case {:?}: {name} must be answered, got {batch}", row.case));
        assert_reported(row.case, "BatchGetCrawlers", crawler, row);
    }
}

/// The read REPORTS: the members the service fills in are the same on every read, and a read
/// of a crawler leaves the next read of it answering exactly the same crawler.
#[tokio::test]
async fn the_filled_members_are_the_same_on_every_read() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    let row = &rows()[0];
    create(&router, "reported-twice", row).await;

    let first = ok(&router, "GetCrawler", &json!({ "Name": "reported-twice" })).await;
    let second = ok(&router, "GetCrawler", &json!({ "Name": "reported-twice" })).await;
    assert_eq!(
        first, second,
        "a read must not change what a later read sees; got {first} then {second}"
    );
    assert_reported(row.case, "the second GetCrawler", &second["Crawler"], row);
}

/// An UPDATE that states one of the defaulted sub-structures replaces it, and one that
/// states none leaves the crawler reporting the value it is in force with — so what a read
/// reports after a write is the definition the service holds, not a record of which members
/// the write mentioned.
#[tokio::test]
async fn an_update_leaves_the_read_reporting_the_definition_in_force() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    // (case, the members the update carries beside the crawler's name, what the read must
    // then report for the policy and the role)
    let cases: Vec<(&str, Value, Value, &str)> = vec![
        (
            "an update that states no policy at all",
            json!({ "Description": "unchanged policies" }),
            defaulted_substructures()["SchemaChangePolicy"].clone(),
            "GlueCrawlerRole",
        ),
        (
            "an update that states one member of the policy",
            json!({ "SchemaChangePolicy": { "DeleteBehavior": "LOG" } }),
            json!({ "UpdateBehavior": "UPDATE_IN_DATABASE", "DeleteBehavior": "LOG" }),
            "GlueCrawlerRole",
        ),
        (
            "an update that replaces the role with an ARN under a path",
            json!({ "Role": "arn:aws:iam::000000000000:role/service-role/AWSGlueServiceRole-v3" }),
            defaulted_substructures()["SchemaChangePolicy"].clone(),
            "AWSGlueServiceRole-v3",
        ),
    ];

    for (index, (case, update, policy, role)) in cases.iter().enumerate() {
        let name = format!("reported-updated-{index}");
        create(&router, &name, &rows()[0]).await;

        let mut request = update.clone();
        request["Name"] = json!(name);
        ok(&router, "UpdateCrawler", &request).await;

        let read = ok(&router, "GetCrawler", &json!({ "Name": name })).await;
        let crawler = &read["Crawler"];
        assert_eq!(
            crawler.get("SchemaChangePolicy"),
            Some(policy),
            "case {case:?}: expected the policy in force {policy}, got {read}"
        );
        assert_eq!(
            crawler.get("Role").and_then(Value::as_str),
            Some(*role),
            "case {case:?}: expected the role {role:?}, got {read}"
        );
        // The other three sub-structures and every target list are reported whatever the
        // update said.
        for member in [
            "RecrawlPolicy",
            "LineageConfiguration",
            "LakeFormationConfiguration",
            "TableProfilingConfiguration",
        ] {
            assert!(
                crawler.get(member).is_some(),
                "case {case:?}: expected {member} to be reported, got {read}"
            );
        }
        for list in TARGET_LISTS {
            assert!(
                crawler["Targets"].get(list).is_some(),
                "case {case:?}: expected the target list {list} to be reported, got {read}"
            );
        }
    }
}
