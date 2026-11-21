// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the readiness a calculated attribute definition reports while the
//! indexing of its historical data is still owed.
//!
//! The rule under test: a calculated attribute definition's `Status` and `Readiness` are a
//! function of the clock alone. A definition with historical data to index reports
//! `PREPARING` with `ProgressPercentage` below full for the whole span the indexing takes,
//! then `COMPLETED` at full progress once that span has passed, with no further call made
//! to make it so; a definition with nothing to index is `COMPLETED` at full progress from
//! its first read and stays so. Progress stays inside the 0..=100 the model declares for
//! `Readiness$ProgressPercentage` and never goes backwards, and reading a definition
//! changes nothing a later caller sees.
//!
//! The sibling suite `calculated_attribute_definitions` asserts the two endpoints of that
//! span (the status at the create, and the status after it); this one asserts what holds AT
//! EVERY instant in between, over a set of reads taken while the indexing is still owed
//! rather than one.
//!
//! Nothing here waits a fixed time: the in-flight reads are taken as fast as the emulator
//! answers them, and the wait for the transition the clock owes polls for that CONDITION
//! through the request helpers' own poll (`calculated_attribute_definitions`).
//!
//! Every case goes through the real router, so what is asserted is the wire behaviour a
//! client sees. The HTTP plumbing is this crate's one test harness, in
//! `tests/all/domain_profile_lifecycle.rs`, and the request helpers are the ones
//! `tests/all/calculated_attribute_definitions.rs` already defines.

use serde_json::{json, Value};

use crate::calculated_attribute_definitions::{
    attribute_details, create, create_domain_with_object_type, definition, get,
    poll_until_status,
};
use crate::domain_profile_lifecycle::Emulator;

/// The `Status` of a definition whose historical data is still being indexed, and of one
/// whose indexing is done — the two values of the modelled `ReadinessStatus` this service
/// reports for a definition that was accepted.
const PREPARING: &str = "PREPARING";
const COMPLETED: &str = "COMPLETED";

/// Full `Readiness$ProgressPercentage`: the maximum of the range the model declares for it.
const FULL_PROGRESS: i64 = 100;

/// How many times each definition is read while the indexing may still be owed. Several
/// reads of the SAME span at whatever instants the host gets to them, which is what the rule
/// generalises over; no read waits for a clock, so a slow host takes the same path with the
/// span further along.
const IN_FLIGHT_READS: usize = 4;

/// The `Status` a response reports.
fn status_of(body: &Value) -> Option<&str> {
    body.get("Status").and_then(Value::as_str)
}

/// The `Readiness$ProgressPercentage` a response reports.
fn progress_of(body: &Value) -> Option<i64> {
    body.get("Readiness")
        .and_then(|readiness| readiness.get("ProgressPercentage"))
        .and_then(Value::as_i64)
}

/// A request that asks for historical data to be included, so the definition has something
/// to index and therefore a span to be `PREPARING` for.
fn historical(statistic: &str) -> Value {
    json!({
        "AttributeDetails": attribute_details("Orders", &["Amount"]),
        "Statistic": statistic,
        "UseHistoricalData": true,
    })
}

/// One row of the table: a definition to create, and whether it has historical data to
/// index — which is the only thing the rule's two halves turn on.
struct Row {
    case: &'static str,
    name: &'static str,
    body: Value,
    indexing: bool,
}

/// The definitions the table below creates: rows with historical data to index, spanning
/// statistic and how much optional configuration the request carries, and rows with nothing
/// to index, spanning the two ways a request declines historical data.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the required members and historical data",
            name: "Span_Minimal",
            body: historical("SUM"),
            indexing: true,
        },
        Row {
            case: "historical data under a different statistic",
            name: "Span_Count",
            body: historical("COUNT"),
            indexing: true,
        },
        Row {
            case: "historical data alongside every optional member",
            name: "Span_Configured",
            body: json!({
                "AttributeDetails": attribute_details("Orders", &["Amount", "Discount"]),
                "Statistic": "AVERAGE",
                "DisplayName": "Order spread",
                "Description": "the spread of every order",
                "Conditions": { "Range": { "Value": 30, "Unit": "DAYS" } },
                "UseHistoricalData": true,
                "Tags": { "team": "crm" },
            }),
            indexing: true,
        },
        Row {
            case: "UseHistoricalData omitted, so nothing to index",
            name: "Instant_Omitted",
            body: definition("SUM"),
            indexing: false,
        },
        Row {
            case: "UseHistoricalData false, so nothing to index",
            name: "Instant_False",
            body: json!({
                "AttributeDetails": attribute_details("Orders", &["Amount"]),
                "Statistic": "MAXIMUM",
                "UseHistoricalData": false,
            }),
            indexing: false,
        },
    ]
}

/// RULE, over the whole span rather than at its endpoints: for as long as the indexing of a
/// definition's historical data is owed, every read reports `PREPARING` with progress inside
/// 0..100 and never below what an earlier read reported, while a definition with nothing to
/// index is already `COMPLETED` at full progress; once the span has passed every read
/// reports `COMPLETED` at full progress, with no call made to bring that about. No read
/// moves anything the create stored.
///
/// Every row is created at the start and read repeatedly, so the one span covers the whole
/// table instead of a span being waited out per row, and no read waits a fixed time: what a
/// read must report follows from the status it reports.
#[tokio::test]
async fn readiness_follows_the_clock_for_the_whole_indexing_span() {
    let emulator = Emulator::new();
    create_domain_with_object_type(&emulator, "readydom", 30).await;

    let table = rows();
    let mut created: Vec<Value> = Vec::new();
    for row in &table {
        let (status, response) = create(&emulator, "readydom", row.name, row.body.clone()).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 for {:?}, got {status} {response:?}",
            row.case, row.body
        );
        // A definition with nothing to index has nothing to wait for, so it is ready in the
        // create's own answer.
        if !row.indexing {
            assert_eq!(
                (status_of(&response), progress_of(&response)),
                (Some(COMPLETED), Some(FULL_PROGRESS)),
                "case {:?}: expected the create to report {COMPLETED} at full progress, \
                 got {response:?}",
                row.case
            );
        }
        created.push(response);
    }

    // Progress may only ever rise, so the highest value each definition has reported so far
    // is the floor its next read has to clear.
    let mut floor: Vec<i64> = vec![0; table.len()];

    // Several reads of each definition while the indexing may still be owed, taken as fast
    // as the emulator answers them. What each read must report follows from the status it
    // reports, never from how far the host has got through the span: while the indexing is
    // owed the definition is PREPARING below full progress, and once the clock has made it
    // due it is COMPLETED at full progress — so neither a fast nor a slow host changes which
    // assertion applies.
    for round in 0..IN_FLIGHT_READS {
        for (index, row) in table.iter().enumerate() {
            let case = row.case;
            let (status, read) = get(&emulator, "readydom", row.name).await;
            assert_eq!(
                status, 200,
                "case {case:?} read {round}: expected 200, got {status} {read:?}"
            );

            let progress = progress_of(&read).unwrap_or_else(|| {
                panic!("case {case:?} read {round}: expected a ProgressPercentage, got {read:?}")
            });
            match status_of(&read) {
                Some(PREPARING) => {
                    assert!(
                        row.indexing,
                        "case {case:?} read {round}: a definition with nothing to index is \
                         never {PREPARING}, got {read:?}"
                    );
                    assert!(
                        (0..FULL_PROGRESS).contains(&progress),
                        "case {case:?} read {round}: expected progress inside \
                         0..{FULL_PROGRESS} while the indexing is still owed, got {progress:?}"
                    );
                }
                Some(COMPLETED) => assert_eq!(
                    progress, FULL_PROGRESS,
                    "case {case:?} read {round}: expected full progress once the indexing is \
                     due, got {read:?}"
                ),
                other => panic!(
                    "case {case:?} read {round}: expected {PREPARING} or {COMPLETED}, got \
                     {other:?}"
                ),
            }
            assert!(
                progress >= floor[index],
                "case {case:?} read {round}: progress must never go backwards, got \
                 {progress:?} after {:?}",
                floor[index]
            );
            floor[index] = progress;

            // The read is a report: it moves neither the instant the definition was created
            // nor anything else the create stored.
            for member in ["CreatedAt", "AttributeDetails", "Statistic"] {
                assert_eq!(
                    read.get(member),
                    created[index].get(member),
                    "case {case:?} read {round}: expected the {member} the create stored, \
                     got {:?} after {:?}",
                    read.get(member),
                    created[index].get(member)
                );
            }
        }
    }

    // The indexing is owed to the clock, so every row reaches COMPLETED with no call made to
    // bring it about: this waits for that CONDITION, row by row, and then asserts it stays
    // due and that the transition moved nothing else.
    for (index, row) in table.iter().enumerate() {
        let case = row.case;
        let settled = poll_until_status(&emulator, "readydom", row.name, COMPLETED).await;
        for (round, read) in [
            ("the read that first saw it due", settled),
            (
                "the read after that",
                get(&emulator, "readydom", row.name).await.1,
            ),
        ] {
            assert_eq!(
                (status_of(&read), progress_of(&read)),
                (Some(COMPLETED), Some(FULL_PROGRESS)),
                "case {case:?} / {round}: expected {COMPLETED} at full progress once the \
                 indexing is due, got {read:?}"
            );
            assert_eq!(
                read.get("CreatedAt"),
                created[index].get("CreatedAt"),
                "case {case:?} / {round}: the materialised transition must not move \
                 CreatedAt, got {:?} after {:?}",
                read.get("CreatedAt"),
                created[index].get("CreatedAt")
            );
        }
    }
}
