// SPDX-License-Identifier: CC0-1.0

//! What `DescribeLimits` reports: the provisioned-capacity quotas of this account in the
//! region the request addresses.
//!
//! The rule these tests pin:
//!
//! > `DescribeLimits` answers the four provisioned-capacity quotas in force for the
//! > account in the region the request addresses — the documented default quotas, each
//! > raised to what that region has ALREADY provisioned (the per-account pair to the
//! > aggregate over every table and global secondary index there, the per-table pair to
//! > the largest single table's own capacity plus its indexes') — with the per-table
//! > quota never above the per-account one, and without changing anything a later caller
//! > sees.
//!
//! One provisioning layout is one row of [`LAYOUTS`]. The rows differ along what the rule
//! generalises over: how much capacity a region holds relative to each documented default
//! (none at all, on-demand only, far below, AT the per-table default, past the per-account
//! default through several tables), how that capacity is spread over tables and indexes, and
//! which region holds it — because the quotas are per region, so one region's provisioning
//! must not move another's.
//!
//! No row provisions a SINGLE table past the per-table default, because no call admits one:
//! that quota is enforced on every declaration of a table's capacity
//! (`crate::table_capacity_quota_refusals`), so the per-table pair is raised only as far as a
//! table the region does hold, and the per-account pair is passed by several tables together.
//!
//! The default quotas themselves come from the crate, not from literals here, so a row
//! below them states the rule rather than one pair of numbers.

use serde_json::{json, Value};
use service_com_amazonaws_dynamodb_dynamodb_20120810::state::{
    DEFAULT_ACCOUNT_MAX_READ_CAPACITY_UNITS, DEFAULT_ACCOUNT_MAX_WRITE_CAPACITY_UNITS,
    DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS, DEFAULT_TABLE_MAX_WRITE_CAPACITY_UNITS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call_in, emulator, table_when, ACTIVE_DEADLINE, POLL_INTERVAL};

/// One table to seed: its own provisioned capacity, and one pair per global secondary
/// index on it.
struct TableSpec {
    name: &'static str,
    /// `(ReadCapacityUnits, WriteCapacityUnits)` the table itself is provisioned for, or
    /// `None` for a `PAY_PER_REQUEST` table, which provisions none.
    provisioned: Option<(i64, i64)>,
    /// The capacity of each global secondary index, in declaration order.
    indexes: &'static [(i64, i64)],
}

/// The four figures an answer carries.
#[derive(Debug, PartialEq, Eq)]
struct Quotas {
    account_read: i64,
    account_write: i64,
    table_read: i64,
    table_write: i64,
}

/// One region's provisioning layout and the quotas it must therefore be reported.
struct Layout {
    case: &'static str,
    region: &'static str,
    tables: &'static [TableSpec],
    expected: Quotas,
}

/// Every layout, each in its own region so the regions can be cross-checked against one
/// another afterwards.
const LAYOUTS: &[Layout] = &[
    Layout {
        case: "a region holding no table at all",
        region: "eu-west-1",
        tables: &[],
        expected: Quotas {
            account_read: DEFAULT_ACCOUNT_MAX_READ_CAPACITY_UNITS,
            account_write: DEFAULT_ACCOUNT_MAX_WRITE_CAPACITY_UNITS,
            table_read: DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS,
            table_write: DEFAULT_TABLE_MAX_WRITE_CAPACITY_UNITS,
        },
    },
    Layout {
        case: "an on-demand table, which provisions no capacity",
        region: "us-east-1",
        tables: &[TableSpec {
            name: "OnDemand",
            provisioned: None,
            indexes: &[],
        }],
        expected: Quotas {
            account_read: DEFAULT_ACCOUNT_MAX_READ_CAPACITY_UNITS,
            account_write: DEFAULT_ACCOUNT_MAX_WRITE_CAPACITY_UNITS,
            table_read: DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS,
            table_write: DEFAULT_TABLE_MAX_WRITE_CAPACITY_UNITS,
        },
    },
    Layout {
        case: "two provisioned tables and an index, far below either default",
        region: "us-west-2",
        tables: &[
            TableSpec {
                name: "Small",
                provisioned: Some((5, 7)),
                indexes: &[(2, 3)],
            },
            TableSpec {
                name: "Smaller",
                provisioned: Some((1, 1)),
                indexes: &[],
            },
        ],
        expected: Quotas {
            account_read: DEFAULT_ACCOUNT_MAX_READ_CAPACITY_UNITS,
            account_write: DEFAULT_ACCOUNT_MAX_WRITE_CAPACITY_UNITS,
            table_read: DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS,
            table_write: DEFAULT_TABLE_MAX_WRITE_CAPACITY_UNITS,
        },
    },
    Layout {
        // 39,000 + 1,000 read units on one table is exactly the 40,000 per-table default,
        // which is admitted and raises nothing: a quota is raised only by capacity PAST it.
        case: "one table whose read capacity, with its index, is at the per-table default",
        region: "ap-southeast-2",
        tables: &[TableSpec {
            name: "ReadHeavy",
            provisioned: Some((39_000, 1_000)),
            indexes: &[(1_000, 500)],
        }],
        expected: Quotas {
            account_read: DEFAULT_ACCOUNT_MAX_READ_CAPACITY_UNITS,
            account_write: DEFAULT_ACCOUNT_MAX_WRITE_CAPACITY_UNITS,
            table_read: DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS,
            table_write: DEFAULT_TABLE_MAX_WRITE_CAPACITY_UNITS,
        },
    },
    Layout {
        // Three tables of 30,000 read and 25,000 write units each — every one of them
        // inside the per-table default: the aggregate (90,000 read, 75,000 write) passes the
        // per-account READ default and not the write one, so only the quota the region
        // actually passed moves, and the per-table pair stays at its default because no
        // single table passes it.
        case: "several tables whose aggregate passes the per-account default",
        region: "eu-central-1",
        tables: &[
            TableSpec {
                name: "BulkOne",
                provisioned: Some((20_000, 20_000)),
                indexes: &[(10_000, 5_000)],
            },
            TableSpec {
                name: "BulkTwo",
                provisioned: Some((30_000, 25_000)),
                indexes: &[],
            },
            TableSpec {
                name: "BulkThree",
                provisioned: Some((30_000, 25_000)),
                indexes: &[],
            },
        ],
        expected: Quotas {
            account_read: 90_000,
            account_write: DEFAULT_ACCOUNT_MAX_WRITE_CAPACITY_UNITS,
            table_read: DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS,
            table_write: DEFAULT_TABLE_MAX_WRITE_CAPACITY_UNITS,
        },
    },
];

/// The `CreateTable` body for one spec: a simple `Id` key, plus one `KEYS_ONLY` index per
/// capacity pair.
fn create_table_body(spec: &TableSpec) -> Value {
    let mut definitions = vec![json!({"AttributeName": "Id", "AttributeType": "S"})];
    let mut indexes = Vec::new();
    for (position, (read, write)) in spec.indexes.iter().enumerate() {
        let key = format!("Gsi{position}Key");
        definitions.push(json!({"AttributeName": key, "AttributeType": "S"}));
        indexes.push(json!({
            "IndexName": format!("Gsi{position}"),
            "KeySchema": [{"AttributeName": key, "KeyType": "HASH"}],
            "Projection": {"ProjectionType": "KEYS_ONLY"},
            "ProvisionedThroughput": {
                "ReadCapacityUnits": read,
                "WriteCapacityUnits": write,
            },
        }));
    }
    let mut body = json!({
        "TableName": spec.name,
        "KeySchema": [{"AttributeName": "Id", "KeyType": "HASH"}],
        "AttributeDefinitions": definitions,
    });
    match spec.provisioned {
        Some((read, write)) => {
            body["BillingMode"] = json!("PROVISIONED");
            body["ProvisionedThroughput"] =
                json!({"ReadCapacityUnits": read, "WriteCapacityUnits": write});
        }
        None => body["BillingMode"] = json!("PAY_PER_REQUEST"),
    }
    if !indexes.is_empty() {
        body["GlobalSecondaryIndexes"] = json!(indexes);
    }
    body
}

/// Seed every one of `tables` in `region`.
async fn seed(router: &MultiServiceRouter, case: &str, region: &str, tables: &[TableSpec]) {
    for spec in tables {
        let body = create_table_body(spec);
        let (status, created) = call_in(router, region, "CreateTable", body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: seeding {} expected HTTP 200, got {status} with {created}",
            spec.name
        );
    }
}

/// The quotas `DescribeLimits` reports in `region`, with the modelled shape checked: four
/// longs, each at least 1 (`PositiveLongObject`, range 1..), and a per-table quota no
/// larger than the per-account one.
async fn reported_quotas(router: &MultiServiceRouter, case: &str, region: &str) -> Quotas {
    let (status, body) = call_in(router, region, "DescribeLimits", json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeLimits expected HTTP 200, got {status} with {body}"
    );
    let value = |member: &str| -> i64 {
        let reported = body[member].as_i64().unwrap_or_else(|| {
            panic!("case {case:?}: {member} is a long, got {body}");
        });
        assert!(
            reported >= 1,
            "case {case:?}: {member} is a positive long, got {reported}"
        );
        reported
    };
    let quotas = Quotas {
        account_read: value("AccountMaxReadCapacityUnits"),
        account_write: value("AccountMaxWriteCapacityUnits"),
        table_read: value("TableMaxReadCapacityUnits"),
        table_write: value("TableMaxWriteCapacityUnits"),
    };
    assert!(
        quotas.table_read <= quotas.account_read && quotas.table_write <= quotas.account_write,
        "case {case:?}: a table's capacity is part of the account's aggregate, so the \
         per-table quota cannot exceed the per-account one, got {quotas:?}"
    );
    quotas
}

#[tokio::test]
async fn limits_are_the_defaults_raised_to_what_the_region_has_provisioned() {
    let router = emulator();

    for layout in LAYOUTS {
        seed(&router, layout.case, layout.region, layout.tables).await;
        let reported = reported_quotas(&router, layout.case, layout.region).await;
        assert_eq!(
            reported, layout.expected,
            "case {:?} in {}: the documented defaults raised to what the region has \
             provisioned, expected {:?}, got {reported:?}",
            layout.case, layout.region, layout.expected
        );
    }

    // Every region again, now that all of them hold their tables: a throughput quota is
    // held per region, so what one region provisioned leaves every other region's report
    // exactly where it was — and the repeated read answers the same figures.
    for layout in LAYOUTS {
        let reported = reported_quotas(&router, layout.case, layout.region).await;
        assert_eq!(
            reported, layout.expected,
            "case {:?} in {}: one region's provisioning does not move another region's \
             quotas, expected {:?}, got {reported:?}",
            layout.case, layout.region, layout.expected
        );
    }
}

#[tokio::test]
async fn limits_follow_the_capacity_the_store_holds() {
    let router = emulator();
    let region = "us-west-1";
    let defaults = Quotas {
        account_read: DEFAULT_ACCOUNT_MAX_READ_CAPACITY_UNITS,
        account_write: DEFAULT_ACCOUNT_MAX_WRITE_CAPACITY_UNITS,
        table_read: DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS,
        table_write: DEFAULT_TABLE_MAX_WRITE_CAPACITY_UNITS,
    };

    let before = reported_quotas(&router, "before any table", region).await;
    assert_eq!(
        before, defaults,
        "case {:?}: a region that has provisioned nothing is on the documented defaults, \
         expected {defaults:?}, got {before:?}",
        "before any table"
    );

    // Tables provisioned past the per-ACCOUNT default together raise that report, which is
    // what makes the answer a function of the store rather than a fixed pair of numbers.
    // None of them is past the per-table default, because no call admits one.
    let bulk: &[TableSpec] = &[
        TableSpec {
            name: "BulkOne",
            provisioned: Some((30_000, 1_000)),
            indexes: &[(5_000, 1_000)],
        },
        TableSpec {
            name: "BulkTwo",
            provisioned: Some((30_000, 1_000)),
            indexes: &[],
        },
        TableSpec {
            name: "BulkThree",
            provisioned: Some((30_000, 1_000)),
            indexes: &[],
        },
    ];
    seed(&router, "tables past the per-account default", region, bulk).await;
    let raised = reported_quotas(&router, "tables past the per-account default", region).await;
    assert_eq!(
        raised,
        Quotas {
            account_read: 95_000,
            account_write: DEFAULT_ACCOUNT_MAX_WRITE_CAPACITY_UNITS,
            table_read: DEFAULT_TABLE_MAX_READ_CAPACITY_UNITS,
            table_write: DEFAULT_TABLE_MAX_WRITE_CAPACITY_UNITS,
        },
        "case {:?}: 95,000 provisioned read units across the region raise the per-account read \
         quota and no other, got {raised:?}",
        "tables past the per-account default"
    );

    // And once one of them is gone, the capacity it held is no longer provisioned, so the
    // report falls back to the documented defaults. A table can only be deleted once it has
    // finished being created, so the delete waits for the CONDITION — the table reporting
    // itself `ACTIVE` — rather than for a span.
    table_when(&router, "before the delete", region, "BulkOne", |table| {
        table["TableStatus"] == json!("ACTIVE")
    })
    .await;
    let (status, deleted) =
        call_in(&router, region, "DeleteTable", json!({"TableName": "BulkOne"})).await;
    assert_eq!(
        status, 200,
        "deleting the table expected HTTP 200, got {status} with {deleted}"
    );
    // The capacity a deleted table held stops being provisioned when the clock makes its
    // deletion due, so the report is READ AGAIN until it answers the defaults.
    let after = quotas_when(&router, "after the table is gone", region, &defaults).await;
    assert_eq!(
        after, defaults,
        "case {:?}: capacity a deleted table held is no longer provisioned, expected \
         {defaults:?}, got {after:?}",
        "after the table is gone"
    );
}

/// Poll `DescribeLimits` in `region` until it reports `settled`, answering the last report it
/// gave — the waiter for a quota the CLOCK owes a change to, bounded by
/// [`crate::emulator::ACTIVE_DEADLINE`] so a report that never settles fails naming what it
/// last said rather than hanging.
async fn quotas_when(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    settled: &Quotas,
) -> Quotas {
    let started = std::time::Instant::now();
    loop {
        let reported = reported_quotas(router, case, region).await;
        if &reported == settled || started.elapsed() >= ACTIVE_DEADLINE {
            return reported;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn describing_limits_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    let region = "ca-central-1";
    seed(
        &router,
        "a reporting read",
        region,
        &[TableSpec {
            name: "Reported",
            provisioned: Some((7, 9)),
            indexes: &[(3, 4)],
        }],
    )
    .await;

    let describe = json!({"TableName": "Reported"});
    let (_, listed_before) = call_in(&router, region, "ListTables", json!({})).await;
    let (_, described_before) = call_in(&router, region, "DescribeTable", describe.clone()).await;

    let first = reported_quotas(&router, "a reporting read", region).await;
    let second = reported_quotas(&router, "a reporting read", region).await;
    assert_eq!(
        first, second,
        "case {:?}: two reads in a row report the same quotas, got {first:?} then {second:?}",
        "a reporting read"
    );

    let (_, listed_after) = call_in(&router, region, "ListTables", json!({})).await;
    let (_, described_after) = call_in(&router, region, "DescribeTable", describe).await;
    assert_eq!(
        listed_before, listed_after,
        "case {:?}: reporting the quotas left the region's tables as they were, got \
         {listed_before} then {listed_after}",
        "a reporting read"
    );
    assert_eq!(
        described_before["Table"]["ProvisionedThroughput"],
        described_after["Table"]["ProvisionedThroughput"],
        "case {:?}: reporting the quotas consumed no capacity and moved no provisioning, \
         got {described_before} then {described_after}",
        "a reporting read"
    );
}
