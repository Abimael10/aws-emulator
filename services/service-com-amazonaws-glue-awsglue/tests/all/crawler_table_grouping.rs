// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW MANY tables a crawl writes for one Amazon S3 include path, and
//! what their columns and partitions are.
//!
//! The rule under test: a crawl describes one candidate table per child prefix of an include
//! path that holds data and then decides whether those candidates are ONE table of the include
//! path or one table each. By DEFAULT they are one table — named after the include path's leaf
//! folder, with the child level as a partition key — only where the schemas at that folder
//! level are SIMILAR: the same classification, and at least 70% (the partition threshold) of
//! the fields either carries are fields both carry under the same name and the same type;
//! candidates that are not similar are a table each, named after their own child prefix and
//! partitioned by the folder levels below it. A crawler whose `Configuration` carries
//! `Grouping.TableGroupingPolicy = CombineCompatibleSchemas` folds candidates whose schemas are
//! merely COMPATIBLE — the same classification, and every field they share read as types that
//! widen to one (a whole number beside a fraction widens to the fraction) — into that single
//! table, whose columns are the candidates' schemas merged. `CreateCrawler$Configuration` is
//! "crawler configuration information. This versioned JSON string allows users to specify
//! aspects of a crawler's behavior", a document the model does not constrain, so a
//! configuration that states no policy, states one this service does not act on, or is not a
//! JSON document at all leaves the default in force.
//!
//! The rows of the table below differ along what the rule generalises over: whether the child
//! prefixes' schemas are identical, similar, dissimilar or incompatible, which policy the
//! `Configuration` states (none, the combining one, an unknown one, a configuration that is not
//! a document), how deep the data sits under each child prefix, whether the data sits directly
//! under the include path at all, and whether the crawler carries a `TablePrefix`. The recorded
//! case — two child prefixes whose schemas are data-compatible but not similar, crawled once
//! under the default policy and once under `CombineCompatibleSchemas` — is the first two rows,
//! and the RELATION between them (the grouped crawler's table count is strictly smaller over
//! the identical include path) is asserted on its own at the end. Both ceilings that bound the
//! grouping's own work are exercised at the first count past them by the last test.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness and the objects are
//! staged through Amazon S3's own `PutObject`, so what is asserted is HTTP behaviour over state
//! the data plane really holds. The due instant a crawl's finish falls at is moved THROUGH THE
//! STORE ([`crate::crawler_runs::set_due_instant`]) rather than slept for.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    crawl_target_unreadable, fold_name, ComAmazonawsGlueAwsglueState, LAST_CRAWL_STATUS_FAILED,
    MAX_CRAWLED_SCHEMA_COLUMNS, MAX_CRAWLED_TABLE_CANDIDATES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::crawler_runs::{already_past, given_database, set_due_instant, stage, ROLE};
use crate::wire::{ok, table_names, ACCOUNT_ID};

/// The include path every crawler here is pointed at, under the row's own bucket.
const INCLUDE_PATH: &str = "data";

/// The `Configuration` that folds compatible schemas into one table, written out here rather
/// than imported from the crate, so a wrong constant in the implementation cannot make this
/// suite pass.
const COMBINING_CONFIGURATION: &str =
    r#"{"Version":1.0,"Grouping":{"TableGroupingPolicy":"CombineCompatibleSchemas"}}"#;

/// One row: the objects staged under the include path, the crawler's `Configuration` and
/// `TablePrefix`, and the tables the crawl must leave behind.
struct Grouping {
    /// What the row is about.
    case: &'static str,
    /// The bucket the row's objects live in (no underscore: `CreateBucket` refuses one).
    bucket: &'static str,
    /// The objects staged, as `(key, body)`.
    objects: Vec<(&'static str, &'static str)>,
    /// The `Configuration` the crawler carries, if any.
    configuration: Option<&'static str>,
    /// The `TablePrefix` it carries, if any.
    table_prefix: Option<&'static str>,
    /// The tables the crawl must publish, in name order, as
    /// `(name, columns, partition keys, partitions)`.
    tables: Vec<Table>,
}

/// One table a row expects, as the catalog must report it.
struct Table {
    /// The table's name, the crawler's `TablePrefix` included.
    name: String,
    /// Its columns, as `(name, type)`, in the order the table declares them.
    columns: Vec<(&'static str, &'static str)>,
    /// Its partition keys, as `(name, type)`.
    partition_keys: Vec<(&'static str, &'static str)>,
    /// The partitions it holds, as the value lists that address them.
    partitions: Vec<Vec<&'static str>>,
}

/// The data of the recorded case: two child prefixes of one include path whose schemas are
/// data-compatible (the whole number widens to the fraction) but not similar (they agree on one
/// field out of three).
fn incompatible_siblings() -> Vec<(&'static str, &'static str)> {
    vec![
        ("data/part1/rows.csv", "id,name\n1,alpha\n"),
        ("data/part2/rows.csv", "id,name,extra\n1.5,beta,x\n"),
    ]
}

/// The two tables the DEFAULT policy leaves behind for [`incompatible_siblings`]: one per child
/// prefix, each with the columns its own data carries and no partition of its own. `prefix` is
/// the crawler's `TablePrefix`, which the names a crawl writes carry.
fn split_tables(prefix: &str) -> Vec<Table> {
    vec![
        Table {
            name: format!("{prefix}part1"),
            columns: vec![("id", "bigint"), ("name", "string")],
            partition_keys: Vec::new(),
            partitions: Vec::new(),
        },
        Table {
            name: format!("{prefix}part2"),
            columns: vec![("id", "double"), ("name", "string"), ("extra", "string")],
            partition_keys: Vec::new(),
            partitions: Vec::new(),
        },
    ]
}

/// The rows.
fn rows() -> Vec<Grouping> {
    vec![
        Grouping {
            case: "the default policy splits child prefixes whose schemas are not similar",
            bucket: "crawl-group-default",
            objects: incompatible_siblings(),
            configuration: None,
            table_prefix: None,
            tables: split_tables(""),
        },
        Grouping {
            case: "CombineCompatibleSchemas folds the same two prefixes into one table",
            bucket: "crawl-group-combined",
            objects: incompatible_siblings(),
            configuration: Some(COMBINING_CONFIGURATION),
            table_prefix: None,
            tables: vec![Table {
                // Named after the include path's leaf folder, with the child level as its
                // partition key and the widened schema of both prefixes.
                name: INCLUDE_PATH.to_string(),
                columns: vec![("id", "double"), ("name", "string"), ("extra", "string")],
                partition_keys: vec![("partition_0", "string")],
                partitions: vec![vec!["part1"], vec!["part2"]],
            }],
        },
        Grouping {
            case: "the split tables carry the crawler's TablePrefix",
            bucket: "crawl-group-prefixed",
            objects: incompatible_siblings(),
            configuration: None,
            table_prefix: Some("p_"),
            tables: split_tables("p_"),
        },
        Grouping {
            case: "identical schemas are one partitioned table under the default policy",
            bucket: "crawl-group-identical",
            objects: vec![
                ("data/part1/rows.csv", "id,name\n1,alpha\n"),
                ("data/part2/rows.csv", "id,name\n2,beta\n"),
            ],
            configuration: None,
            table_prefix: None,
            tables: vec![Table {
                name: INCLUDE_PATH.to_string(),
                columns: vec![("id", "bigint"), ("name", "string")],
                partition_keys: vec![("partition_0", "string")],
                partitions: vec![vec!["part1"], vec!["part2"]],
            }],
        },
        Grouping {
            case: "schemas agreeing on more than the partition threshold are one table",
            bucket: "crawl-group-similar",
            objects: vec![
                // Four of the five fields either carries are shared under the same name and
                // type, which is above the threshold.
                (
                    "data/part1/rows.csv",
                    "a,b,c,d\n1,alpha,true,2.5\n",
                ),
                (
                    "data/part2/rows.csv",
                    "a,b,c,d,e\n2,beta,false,3.5,x\n",
                ),
            ],
            configuration: None,
            table_prefix: None,
            tables: vec![Table {
                name: INCLUDE_PATH.to_string(),
                columns: vec![
                    ("a", "bigint"),
                    ("b", "string"),
                    ("c", "boolean"),
                    ("d", "double"),
                    ("e", "string"),
                ],
                partition_keys: vec![("partition_0", "string")],
                partitions: vec![vec!["part1"], vec!["part2"]],
            }],
        },
        Grouping {
            case: "CombineCompatibleSchemas does not fold a shared field nothing widens",
            bucket: "crawl-group-unwidenable",
            objects: vec![
                ("data/part1/rows.csv", "id,name\n1,alpha\n"),
                ("data/part2/rows.csv", "id,name\ntext,beta\n"),
            ],
            configuration: Some(COMBINING_CONFIGURATION),
            table_prefix: None,
            tables: vec![
                Table {
                    name: "part1".to_string(),
                    columns: vec![("id", "bigint"), ("name", "string")],
                    partition_keys: Vec::new(),
                    partitions: Vec::new(),
                },
                Table {
                    name: "part2".to_string(),
                    columns: vec![("id", "string"), ("name", "string")],
                    partition_keys: Vec::new(),
                    partitions: Vec::new(),
                },
            ],
        },
        Grouping {
            case: "a split table keeps the partitions of the folder levels below its own prefix",
            bucket: "crawl-group-deep",
            objects: vec![
                ("data/part1/day=01/rows.csv", "id,name\n1,alpha\n"),
                ("data/part1/day=02/rows.csv", "id,name\n2,beta\n"),
                ("data/part2/day=01/rows.csv", "id,name,extra\n1.5,gamma,x\n"),
            ],
            configuration: None,
            table_prefix: None,
            tables: vec![
                Table {
                    name: "part1".to_string(),
                    columns: vec![("id", "bigint"), ("name", "string")],
                    partition_keys: vec![("day", "string")],
                    partitions: vec![vec!["01"], vec!["02"]],
                },
                Table {
                    name: "part2".to_string(),
                    columns: vec![("id", "double"), ("name", "string"), ("extra", "string")],
                    partition_keys: vec![("day", "string")],
                    partitions: vec![vec!["01"]],
                },
            ],
        },
        Grouping {
            case: "data directly under the include path is that one table, policy or not",
            bucket: "crawl-group-flat",
            objects: vec![
                ("data/one.csv", "id,name\n1,alpha\n"),
                ("data/two.csv", "id,name\n2,beta\n"),
            ],
            configuration: Some(COMBINING_CONFIGURATION),
            table_prefix: None,
            tables: vec![Table {
                name: INCLUDE_PATH.to_string(),
                columns: vec![("id", "bigint"), ("name", "string")],
                partition_keys: Vec::new(),
                partitions: Vec::new(),
            }],
        },
        Grouping {
            case: "a configuration naming another policy leaves the default in force",
            bucket: "crawl-group-other-policy",
            objects: incompatible_siblings(),
            configuration: Some(r#"{"Version":1.0,"Grouping":{"TableGroupingPolicy":"NoSuch"}}"#),
            table_prefix: None,
            tables: split_tables(""),
        },
        Grouping {
            case: "a configuration that is not a JSON document leaves the default in force",
            bucket: "crawl-group-unreadable",
            objects: incompatible_siblings(),
            configuration: Some("not a document"),
            table_prefix: None,
            tables: split_tables(""),
        },
    ]
}

/// An emulator with Glue AND Amazon S3 on one shared engine, plus the engine: the crawl reads
/// the objects a test stages out of that Amazon S3.
fn emulator_with_s3() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// Create the crawler of one row over its bucket's include path, and crawl it to rest.
async fn crawl_the_include_path(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    row: &Grouping,
    database: &str,
    crawler: &str,
) {
    let mut body = json!({
        "Name": crawler,
        "Role": ROLE,
        "DatabaseName": database,
        "Targets": {
            "S3Targets": [{ "Path": format!("s3://{}/{INCLUDE_PATH}/", row.bucket) }],
        },
    });
    if let Some(configuration) = row.configuration {
        body["Configuration"] = json!(configuration);
    }
    if let Some(prefix) = row.table_prefix {
        body["TablePrefix"] = json!(prefix);
    }
    ok(router, "CreateCrawler", &body).await;
    ok(router, "StartCrawler", &json!({ "Name": crawler })).await;
    set_due_instant(engine, already_past());
}

/// The `(name, type)` pairs of a `Column` list, in the order it holds them.
fn named_types(columns: Option<&Value>) -> Vec<(String, String)> {
    columns
        .and_then(Value::as_array)
        .map(|columns| {
            columns
                .iter()
                .map(|column| {
                    (
                        column["Name"].as_str().unwrap_or_default().to_string(),
                        column["Type"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `Values` of every partition the catalog holds for one table, in the order it holds them.
///
/// Read from the store because the operation that PAGES over a table's partitions
/// (`GetPartitions`) is a scaffold in this crate; every partition this answers is also read
/// back over the wire, one at a time, by [`served_partition`].
fn stored_partitions(
    engine: &Arc<engine::InMemoryEngine>,
    database: &str,
    table: &str,
) -> Vec<Vec<String>> {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let store = state.lock();
    store
        .database(ACCOUNT_ID, database)
        .and_then(|database| database.tables.get(&fold_name(table)))
        .map(|table| table.partitions.keys().cloned().collect())
        .unwrap_or_default()
}

/// Whether `GetPartition` serves the partition one value list addresses.
async fn served_partition(
    router: &MultiServiceRouter,
    database: &str,
    table: &str,
    values: &[String],
) -> bool {
    let (status, _) = crate::wire::call(
        router,
        "GetPartition",
        &json!({
            "DatabaseName": database,
            "TableName": table,
            "PartitionValues": values,
        }),
    )
    .await;
    status == 200
}

/// The expected pairs of a row, as owned strings.
fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, declared)| ((*name).to_string(), (*declared).to_string()))
        .collect()
}

/// How many tables a crawl writes for one include path, and what each of them declares, follows
/// the schemas at the child folder level and the crawler's grouping policy.
#[tokio::test]
async fn a_crawl_groups_the_child_prefixes_of_an_include_path_into_tables() {
    for row in &rows() {
        let case = row.case;
        let (router, engine) = emulator_with_s3();
        let database = format!("{}_db", row.bucket.replace('-', "_"));
        stage(&router, row.bucket, &row.objects).await;
        given_database(&router, &database).await;
        crawl_the_include_path(&router, &engine, row, &database, "crawler").await;

        let listed = ok(
            &router,
            "GetTables",
            &json!({ "DatabaseName": database }),
        )
        .await;
        let expected_names: Vec<String> = row.tables.iter().map(|table| table.name.clone()).collect();
        let actual_names = table_names(&listed);
        assert_eq!(
            actual_names, expected_names,
            "case {case:?}: expected the tables {expected_names:?}, got {actual_names:?}"
        );

        for expected in &row.tables {
            let read = ok(
                &router,
                "GetTable",
                &json!({ "DatabaseName": database, "Name": expected.name }),
            )
            .await;
            let table = &read["Table"];
            let actual_columns = named_types(table.pointer("/StorageDescriptor/Columns"));
            assert_eq!(
                actual_columns,
                owned(&expected.columns),
                "case {case:?}: expected the columns {:?} of {}, got {actual_columns:?}",
                expected.columns,
                expected.name
            );
            let actual_keys = named_types(table.get("PartitionKeys"));
            assert_eq!(
                actual_keys,
                owned(&expected.partition_keys),
                "case {case:?}: expected the partition keys {:?} of {}, got {actual_keys:?}",
                expected.partition_keys,
                expected.name
            );
            // The table is located at the prefix its own data sits under, which is the include
            // path for a folded table and the child prefix for a split one.
            let location = table
                .pointer("/StorageDescriptor/Location")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let leaf = expected
                .name
                .trim_start_matches(row.table_prefix.unwrap_or_default());
            assert!(
                location.starts_with(&format!("s3://{}/{INCLUDE_PATH}/", row.bucket))
                    && location.ends_with(&format!("{leaf}/")),
                "case {case:?}: expected {} located at the prefix its data sits under, got \
                 {location}",
                expected.name
            );

            let expected_partitions: Vec<Vec<String>> = expected
                .partitions
                .iter()
                .map(|values| values.iter().map(|value| (*value).to_string()).collect())
                .collect();
            let actual_partitions = stored_partitions(&engine, &database, &expected.name);
            assert_eq!(
                actual_partitions, expected_partitions,
                "case {case:?}: expected the partitions {expected_partitions:?} of {}, got \
                 {actual_partitions:?}",
                expected.name
            );
            for values in &expected_partitions {
                assert!(
                    served_partition(&router, &database, &expected.name, values).await,
                    "case {case:?}: the partition {values:?} of {} must be served",
                    expected.name
                );
            }
        }
    }
}

/// The RELATION the grouping policy states: over the IDENTICAL include path, the crawler
/// carrying `CombineCompatibleSchemas` publishes strictly fewer tables than the one that states
/// no policy — the two crawlers differing in nothing else, in one database each.
#[tokio::test]
async fn combining_compatible_schemas_publishes_fewer_tables_than_the_default() {
    let case = "two crawlers over one include path, one of them combining";
    let (router, engine) = emulator_with_s3();
    let row = Grouping {
        case,
        bucket: "crawl-group-relation",
        objects: incompatible_siblings(),
        configuration: None,
        table_prefix: None,
        tables: Vec::new(),
    };
    stage(&router, row.bucket, &row.objects).await;

    let mut counted: Vec<(&str, usize, Vec<String>)> = Vec::new();
    for (label, configuration) in [
        ("the default policy", None),
        ("CombineCompatibleSchemas", Some(COMBINING_CONFIGURATION)),
    ] {
        let database = format!("crawl_group_relation_{}_db", counted.len());
        given_database(&router, &database).await;
        let crawler = format!("crawl-group-relation-{}", counted.len());
        let configured = Grouping {
            case,
            bucket: row.bucket,
            objects: Vec::new(),
            configuration,
            table_prefix: None,
            tables: Vec::new(),
        };
        crawl_the_include_path(&router, &engine, &configured, &database, &crawler).await;
        let listed = ok(
            &router,
            "GetTables",
            &json!({ "DatabaseName": database }),
        )
        .await;
        let names = table_names(&listed);
        counted.push((label, names.len(), names));
    }

    let (default_label, default_count, default_names) = &counted[0];
    let (grouped_label, grouped_count, grouped_names) = &counted[1];
    assert!(
        grouped_count < default_count,
        "case {case:?}: {grouped_label} produced {grouped_count} tables ({grouped_names:?}) and \
         {default_label} produced {default_count} ({default_names:?}) over the identical include \
         path; the grouped count must be strictly smaller"
    );
}

/// Both factors of the grouping's work are BOUNDED: a target holding more child prefixes than
/// one crawl describes ([`MAX_CRAWLED_TABLE_CANDIDATES`]), and one whose data carries more
/// columns between them than it describes ([`MAX_CRAWLED_SCHEMA_COLUMNS`]), are data the crawl
/// did not describe — so it FAILS naming the target it could not read and writes no table,
/// rather than reporting part of the layout as the whole of it.
///
/// Each ceiling is exercised at the first count past it; the rows of the table above sit well
/// inside both.
#[tokio::test]
async fn a_target_past_a_grouping_ceiling_is_one_the_crawl_did_not_describe() {
    // The first count past each ceiling, and the layout that reaches it.
    let past_the_prefixes: Vec<(String, String)> = (0..=MAX_CRAWLED_TABLE_CANDIDATES)
        .map(|index| {
            (
                format!("{INCLUDE_PATH}/part{index:04}/rows.csv"),
                "id,name\n1,alpha\n".to_string(),
            )
        })
        .collect();
    let wide_header: String = (0..=MAX_CRAWLED_SCHEMA_COLUMNS)
        .map(|index| format!("c{index}"))
        .collect::<Vec<String>>()
        .join(",");
    let past_the_columns: Vec<(String, String)> = vec![(
        format!("{INCLUDE_PATH}/wide/rows.csv"),
        format!("{wide_header}\n"),
    )];

    for (case, bucket, objects) in [
        (
            "one child prefix past the prefix ceiling",
            "crawl-group-many-prefixes",
            past_the_prefixes,
        ),
        (
            "one column past the column ceiling",
            "crawl-group-wide-header",
            past_the_columns,
        ),
    ] {
        let (router, engine) = emulator_with_s3();
        let database = format!("{}_db", bucket.replace('-', "_"));
        let staged: Vec<(&str, &str)> = objects
            .iter()
            .map(|(key, body)| (key.as_str(), body.as_str()))
            .collect();
        stage(&router, bucket, &staged).await;
        given_database(&router, &database).await;
        let row = Grouping {
            case,
            bucket,
            objects: Vec::new(),
            configuration: None,
            table_prefix: None,
            tables: Vec::new(),
        };
        crawl_the_include_path(&router, &engine, &row, &database, "crawler").await;

        let reported = ok(&router, "GetCrawler", &json!({ "Name": "crawler" })).await;
        assert_eq!(
            reported
                .pointer("/Crawler/LastCrawl/Status")
                .and_then(Value::as_str),
            Some(LAST_CRAWL_STATUS_FAILED),
            "case {case:?}: the crawl did not describe the target, got {reported}"
        );
        let target = format!("s3://{bucket}/{INCLUDE_PATH}/");
        assert_eq!(
            reported
                .pointer("/Crawler/LastCrawl/ErrorMessage")
                .and_then(Value::as_str),
            Some(crawl_target_unreadable(&target).as_str()),
            "case {case:?}: the crawl names the target it could not read, got {reported}"
        );
        let listed = ok(&router, "GetTables", &json!({ "DatabaseName": database })).await;
        let names = table_names(&listed);
        assert!(
            names.is_empty(),
            "case {case:?}: a crawl that failed writes no table, got {names:?}"
        );
    }
}
