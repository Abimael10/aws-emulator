// SPDX-License-Identifier: CC0-1.0

//! The ORDER a `Scan` walks what it reads in, and what each row of that walk COSTS.
//!
//! The two rules pinned here, each over a set of inputs rather than one example:
//!
//! 1. a scan walks the KEY SPACE of the table or index it reads: each partition sits where
//!    the hash of its partition key puts it, and the rows of one partition follow in the
//!    order the store holds them. A parallel `Scan` divides that same space into one
//!    contiguous block per segment ("TotalSegments represents the total number of segments
//!    into which the Scan operation will be divided ... Segment identifies an individual
//!    segment to be scanned by an application worker" — `Scan$TotalSegments` /
//!    `Scan$Segment`), so a segment reads a RUN of the scan's own walk: the segments in
//!    segment order answer exactly the unsegmented scan, every item once, and paging inside
//!    one segment carries on along that run. Where a partition falls follows its key alone,
//!    so neither the order items were written in nor how many of them there are moves it;
//! 2. a scan of a SECONDARY INDEX is charged over what the index HOLDS for each row it
//!    evaluated, a whole read unit per 4 KB of each entry, billed on that index's own arm
//!    of `ConsumedCapacity` — while a scan of the TABLE's own items takes the eventually
//!    consistent half unit per 4 KB the request asked to read them with. The rule belongs
//!    to the walk and not to the operation that drives it, so a PartiQL `SELECT` that
//!    walks an index is charged the same way.
//!
//! Every call goes over the wire through the same `MultiServiceRouter` the emulator's front
//! doors use, from `crate::emulator`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{describe_when_active, emulator, ok};
// The tables a rule about what a scan WALKS is read over are seeded once for this test
// binary, beside the suite that reads the segmentation rule over them.
use crate::scan_segment_partitions::{item_keys, seed_shapes, SHAPES};

/// One read capacity unit covers 4 KB of a strongly consistent read ("one read capacity
/// unit represents one strongly consistent read per second ... for an item up to 4 KB" —
/// the Developer Guide's "Read/write capacity mode").
const READ_UNIT_BYTES: usize = 4 * 1024;

#[tokio::test]
async fn the_segments_of_a_parallel_scan_are_runs_of_the_scans_own_walk() {
    let router = emulator();
    seed_shapes(&router).await;

    for shape in &SHAPES {
        let whole = item_keys(&ok(&router, "Scan", json!({"TableName": shape.table})).await, "S");
        let seeded = shape.total();
        assert_eq!(
            whole.len(),
            seeded,
            "case {:?}: the unsegmented scan reads every row, got {whole:?}",
            shape.case
        );
        // Rows: how many segments the scan divides the space into — one (the sequential
        // scan), fewer than the table has partitions, as many, and more than there are
        // rows, which leaves segments with nothing to read.
        for total in [1usize, 2, 3, shape.partitions, seeded + 2] {
            let case = format!("{:?} over {total} segment(s)", shape.case);
            let mut concatenated: Vec<String> = Vec::new();
            let mut evaluated = 0usize;
            for segment in 0..total {
                let page = ok(
                    &router,
                    "Scan",
                    json!({
                        "TableName": shape.table,
                        "Segment": segment,
                        "TotalSegments": total,
                    }),
                )
                .await;
                let keys = item_keys(&page, "S");
                assert_eq!(
                    page["Count"].clone(),
                    json!(keys.len()),
                    "case {case} segment {segment}: Count is what the segment answered, \
                     got {page}"
                );
                assert_eq!(
                    page["ScannedCount"].clone(),
                    json!(keys.len()),
                    "case {case} segment {segment}: with no filter ScannedCount is what \
                     the segment evaluated, got {page}"
                );
                evaluated += keys.len();
                concatenated.extend(keys);
            }
            assert_eq!(
                evaluated, seeded,
                "case {case}: the segments together evaluate every row exactly once, got \
                 {concatenated:?}"
            );
            // The segments are contiguous blocks of the one space the scan walks, so
            // reading them in segment order reads that walk in its own order.
            assert_eq!(
                concatenated, whole,
                "case {case}: the segments in segment order are the scan's own walk, got \
                 {concatenated:?} against {whole:?}"
            );
        }
    }
}

#[tokio::test]
async fn paging_inside_one_segment_carries_on_along_its_run() {
    let router = emulator();
    seed_shapes(&router).await;

    for shape in &SHAPES {
        let seeded = shape.total();
        // Rows: the page size the caller asks for, from one row at a time up to more than
        // the segment can hold.
        for limit in [1usize, 2, seeded + 1] {
            for total in [2usize, 3] {
                let case = format!(
                    "{:?} over {total} segment(s), {limit} row(s) a page",
                    shape.case
                );
                for segment in 0..total {
                    let unpaged = item_keys(
                        &ok(
                            &router,
                            "Scan",
                            json!({
                                "TableName": shape.table,
                                "Segment": segment,
                                "TotalSegments": total,
                            }),
                        )
                        .await,
                        "S",
                    );
                    let mut paged: Vec<String> = Vec::new();
                    let mut start: Option<Value> = None;
                    for _ in 0..=seeded {
                        let mut request = json!({
                            "TableName": shape.table,
                            "Segment": segment,
                            "TotalSegments": total,
                            "Limit": limit,
                        });
                        if let Some(key) = start.clone() {
                            request["ExclusiveStartKey"] = key;
                        }
                        let page = ok(&router, "Scan", request).await;
                        let keys = item_keys(&page, "S");
                        assert!(
                            keys.len() <= limit,
                            "case {case} segment {segment}: a page holds at most the Limit \
                             the request asked for, got {page}"
                        );
                        paged.extend(keys);
                        match page.get("LastEvaluatedKey").filter(|key| !key.is_null()) {
                            Some(key) => start = Some(key.clone()),
                            None => break,
                        }
                    }
                    assert_eq!(
                        paged, unpaged,
                        "case {case} segment {segment}: the pages carry on along the \
                         segment's own run, got {paged:?} against {unpaged:?}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn where_a_partition_falls_follows_its_key_and_not_the_table_it_sits_in() {
    let router = emulator();
    // The same partition keys in two tables, written in opposite orders, with the second
    // table carrying extra keys as well: a partition sits where the hash of its key puts
    // it, so neither the write order nor the other partitions move it.
    let shared: [&str; 4] = ["k0", "k1", "k2", "k3"];
    let extra: [&str; 2] = ["k4", "k5"];
    for (case, table, keys) in [
        ("written in ascending key order", "WalkAscending", shared.to_vec()),
        (
            "written in descending key order, beside other keys",
            "WalkDescending",
            {
                let mut keys: Vec<&str> = shared.to_vec();
                keys.reverse();
                keys.extend(extra);
                keys
            },
        ),
    ] {
        ok(
            &router,
            "CreateTable",
            json!({
                "TableName": table,
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "BillingMode": "PAY_PER_REQUEST",
            }),
        )
        .await;
        describe_when_active(&router, case, table).await;
        for key in keys {
            ok(
                &router,
                "PutItem",
                json!({"TableName": table, "Item": {"pk": {"S": key}}}),
            )
            .await;
        }
    }

    // Which segment each of the shared keys falls in, read off each table by scanning
    // every segment of the same fan-out.
    for total in [2usize, 3, 5] {
        let mut placed: Vec<Vec<usize>> = Vec::new();
        for table in ["WalkAscending", "WalkDescending"] {
            let mut segments: Vec<usize> = Vec::new();
            for key in shared {
                let mut owner: Option<usize> = None;
                for segment in 0..total {
                    let page = ok(
                        &router,
                        "Scan",
                        json!({
                            "TableName": table,
                            "Segment": segment,
                            "TotalSegments": total,
                        }),
                    )
                    .await;
                    let carries = page["Items"]
                        .as_array()
                        .is_some_and(|items| {
                            items.iter().any(|item| item["pk"]["S"] == json!(key))
                        });
                    if carries {
                        assert!(
                            owner.is_none(),
                            "case over {total} segment(s): {key:?} is read by ONE segment \
                             of {table}, but segment {segment} read it again"
                        );
                        owner = Some(segment);
                    }
                }
                segments.push(owner.unwrap_or_else(|| {
                    panic!("case over {total} segment(s): no segment of {table} read {key:?}")
                }));
            }
            placed.push(segments);
        }
        assert_eq!(
            placed.first(),
            placed.last(),
            "case over {total} segment(s): the same keys fall in the same segments in \
             both tables, got {placed:?}"
        );
    }
}

/// The table the capacity rule is driven over, with one index of each kind beside the
/// keys-only index a read of it has to go on to the table from.
const CHARGED: &str = "WalkCharged";

/// How wide the rows of that table are made: one well inside a read unit, one that spills
/// into a second and one that spills into a third, so a per-row rounding is the only thing
/// the figures can be the sum of.
const WIDTHS: [usize; 3] = [0, READ_UNIT_BYTES, 2 * READ_UNIT_BYTES];

/// Create [`CHARGED`] and seed one row per [`WIDTHS`] entry, answering the whole units each
/// row's own size rounds up to.
async fn seed_charged(router: &MultiServiceRouter) -> f64 {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": CHARGED,
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
                {"AttributeName": "gk", "AttributeType": "S"},
                {"AttributeName": "lk", "AttributeType": "N"},
            ],
            "GlobalSecondaryIndexes": [{
                "IndexName": "ByGlobal",
                "KeySchema": [{"AttributeName": "gk", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }],
            "LocalSecondaryIndexes": [
                {
                    "IndexName": "ByLocal",
                    "KeySchema": [
                        {"AttributeName": "pk", "KeyType": "HASH"},
                        {"AttributeName": "lk", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
                {
                    "IndexName": "ByLocalKeys",
                    "KeySchema": [
                        {"AttributeName": "pk", "KeyType": "HASH"},
                        {"AttributeName": "lk", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "KEYS_ONLY"},
                },
            ],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, "a table with an index of each kind", CHARGED).await;

    let mut whole_units = 0.0;
    for (row, width) in WIDTHS.iter().enumerate() {
        ok(
            router,
            "PutItem",
            json!({
                "TableName": CHARGED,
                "Item": {
                    "pk": {"S": format!("c{row}")},
                    "sk": {"N": row.to_string()},
                    "gk": {"S": "g"},
                    "lk": {"N": row.to_string()},
                    "v": {"S": "p".repeat(*width)},
                },
            }),
        )
        .await;
        // The row is at least `width` bytes wide and far short of the next read unit above
        // that, so what it rounds up to is one unit per started 4 KB.
        whole_units += (width / READ_UNIT_BYTES + 1) as f64;
    }
    whole_units
}

/// One charged scan: the index it names (`None` for the table itself), the `Select` beside
/// it, whether it asked to read strongly consistently, the member of the `INDEXES`
/// breakdown its units belong on, and the figure that arm must carry.
struct Charge {
    case: &'static str,
    index: Option<&'static str>,
    select: Option<&'static str>,
    consistent: bool,
    member: &'static str,
    units: f64,
}

#[tokio::test]
async fn a_scan_is_charged_per_row_on_the_arm_that_served_it() {
    let router = emulator();
    let whole_units = seed_charged(&router).await;

    let rows: [Charge; 6] = [
        Charge {
            case: "the table itself, eventually consistent",
            index: None,
            select: None,
            consistent: false,
            member: "Table",
            units: whole_units / 2.0,
        },
        Charge {
            case: "the table itself, strongly consistent",
            index: None,
            select: None,
            consistent: true,
            member: "Table",
            units: whole_units,
        },
        Charge {
            case: "a global secondary index projecting every attribute",
            index: Some("ByGlobal"),
            select: None,
            consistent: false,
            member: "GlobalSecondaryIndexes",
            units: whole_units,
        },
        Charge {
            case: "a local secondary index projecting every attribute",
            index: Some("ByLocal"),
            select: None,
            consistent: false,
            member: "LocalSecondaryIndexes",
            units: whole_units,
        },
        Charge {
            case: "the same local index read strongly consistently",
            index: Some("ByLocal"),
            select: None,
            consistent: true,
            member: "LocalSecondaryIndexes",
            units: whole_units,
        },
        Charge {
            // A keys-only index holds only the keys, so each of its entries is inside one
            // read unit however wide the row it projects is.
            case: "a keys-only local secondary index asked only for what it projects",
            index: Some("ByLocalKeys"),
            select: Some("ALL_PROJECTED_ATTRIBUTES"),
            consistent: false,
            member: "LocalSecondaryIndexes",
            units: WIDTHS.len() as f64,
        },
    ];

    for row in rows {
        let (case, member, expected) = (row.case, row.member, row.units);
        let mut request = json!({
            "TableName": CHARGED,
            "ConsistentRead": row.consistent,
            "ReturnConsumedCapacity": "INDEXES",
        });
        if let Some(index) = row.index {
            request["IndexName"] = json!(index);
        }
        if let Some(select) = row.select {
            request["Select"] = json!(select);
        }
        let scanned = ok(&router, "Scan", request).await;
        let consumed = &scanned["ConsumedCapacity"];
        let arm = match row.index {
            Some(index) => consumed[member][index]["CapacityUnits"].as_f64(),
            None => consumed[member]["CapacityUnits"].as_f64(),
        };
        assert_eq!(
            arm,
            Some(expected),
            "case {case:?}: expected {expected} unit(s) on the {member} arm, got {consumed}"
        );
        // A read served from an index spends nothing on the table unless it had to go on
        // to it, and the aggregate is what the arms carry together.
        let table_arm = consumed["Table"]["CapacityUnits"].as_f64().unwrap_or(0.0);
        if row.index.is_some() {
            assert_eq!(
                table_arm, 0.0,
                "case {case:?}: this read was answered by the index alone, got {consumed}"
            );
        }
        let aggregate = if row.index.is_some() {
            expected + table_arm
        } else {
            expected
        };
        assert_eq!(
            consumed["CapacityUnits"].as_f64(),
            Some(aggregate),
            "case {case:?}: the aggregate is what the arms carry together, got {consumed}"
        );
        assert_eq!(
            consumed["TableName"],
            json!(CHARGED),
            "case {case:?}: the capacity names the table that served the scan, got {consumed}"
        );
    }
}

#[tokio::test]
async fn a_walk_that_evaluated_no_row_is_charged_the_minimum_a_read_takes() {
    let router = emulator();
    // A table with an index of each kind and NO items, so every walk over it evaluates
    // nothing: the per-row charge has no row to apply to, and what is left is the one unit
    // a read takes at least — half of it where the request asked to read eventually
    // consistently, whichever of the three walks it is.
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": "WalkEmpty",
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
                {"AttributeName": "gk", "AttributeType": "S"},
            ],
            "GlobalSecondaryIndexes": [{
                "IndexName": "ByGlobal",
                "KeySchema": [{"AttributeName": "gk", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "an empty table with an index", "WalkEmpty").await;

    // Each row: the index the walk names, whether it asked for a strongly consistent read,
    // and the units it must report.
    for (index, consistent, expected) in [
        (None, false, 0.5),
        (None, true, 1.0),
        (Some("ByGlobal"), false, 0.5),
    ] {
        let mut request = json!({
            "TableName": "WalkEmpty",
            "ConsistentRead": consistent,
            "ReturnConsumedCapacity": "TOTAL",
        });
        if let Some(index) = index {
            request["IndexName"] = json!(index);
        }
        let scanned = ok(&router, "Scan", request).await;
        assert_eq!(
            scanned["Count"].clone(),
            json!(0),
            "case {index:?} consistent {consistent}: the walk evaluated nothing, got {scanned}"
        );
        assert_eq!(
            scanned["ConsumedCapacity"]["CapacityUnits"].as_f64(),
            Some(expected),
            "case {index:?} consistent {consistent}: expected the {expected}-unit minimum, \
             got {scanned}"
        );
    }
}

#[tokio::test]
async fn a_statement_that_walks_an_index_is_charged_the_way_the_scan_of_it_is() {
    let router = emulator();
    let whole_units = seed_charged(&router).await;

    // The same walk through the PartiQL surface: a `SELECT` with no key predicate walks
    // its source the way `Scan` does, so a source that is a secondary index is charged
    // over what the index holds for each row, in whole units.
    let selected = ok(
        &router,
        "ExecuteStatement",
        json!({
            "Statement": format!("SELECT * FROM \"{CHARGED}\".\"ByGlobal\""),
            "ReturnConsumedCapacity": "INDEXES",
        }),
    )
    .await;
    let consumed = &selected["ConsumedCapacity"];
    assert_eq!(
        consumed["GlobalSecondaryIndexes"]["ByGlobal"]["CapacityUnits"].as_f64(),
        Some(whole_units),
        "case \"a statement walking a global secondary index\": expected {whole_units} \
         unit(s) on the index arm, got {consumed}"
    );
    assert_eq!(
        consumed["CapacityUnits"].as_f64(),
        Some(whole_units),
        "case \"a statement walking a global secondary index\": the aggregate is what the \
         index arm carries, got {consumed}"
    );
}
