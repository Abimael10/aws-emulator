// SPDX-License-Identifier: CC0-1.0

//! How a Smithy `double` is rendered on this service's awsQuery wire, end to end.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter`
//! the production router builds, reusing the harness the cluster-lifecycle suite
//! owns rather than keeping a second copy of it.
//!
//! The RULE every case asserts: a `double` member carries a FRACTIONAL PART
//! whichever value it holds — a whole value reads `0.0` or `1533.0`, never `0`
//! or `1533`, and a value that already has fractional digits keeps the digits it
//! has — while a member the model targets at `Integer` or `Long` carries none at
//! all. It is a property of the TYPE, not of any one operation, so the table
//! spans every operation of this service that puts a double on the wire today
//! and each kind of value, in a top-level structure and nested inside a list.
//!
//! Every expected text is written out here rather than computed with the
//! renderer the service uses, so a wrong renderer cannot agree with this suite.

use crate::cluster_lifecycle::{all_text, element_blocks, Emulator};
use crate::reserved_nodes::{catalog, offering_of, one, purchase, ONE_YEAR};

/// Every element this service serializes a Smithy `double` under, written out
/// here rather than read off the production wiring so a wrong entry fails this
/// suite.
///
/// Source: the `double`-targeting members of this model, read off it shape by
/// shape with `smithy_query(query="shape", shape="…")` — `ReservedNodeOffering`
/// and `ReservedNode` (`FixedPrice`, `UsagePrice`), `RecurringCharge`
/// (`RecurringChargeAmount`), `Snapshot` (`TotalBackupSizeInMegaBytes`,
/// `ActualIncrementalBackupSizeInMegaBytes`, `BackupProgressInMegaBytes`,
/// `CurrentBackupRateInMegaBytesPerSecond`), `CustomerStorageMessage`
/// (`TotalBackupSizeInMegaBytes`, `TotalProvisionedStorageInMegaBytes`),
/// `ResizeProgressMessage` (`AvgResizeRateInMegaBytesPerSecond`,
/// `DataTransferProgressPercent`), `RestoreStatus`
/// (`CurrentRestoreRateInMegaBytesPerSecond`), `DataTransferProgress`
/// (`CurrentRateInMegaBytesPerSecond`) and `NodeConfigurationOption`
/// (`EstimatedDiskUtilizationPercent`).
const DOUBLE_ELEMENTS: [&str; 13] = [
    "FixedPrice",
    "UsagePrice",
    "RecurringChargeAmount",
    "TotalBackupSizeInMegaBytes",
    "ActualIncrementalBackupSizeInMegaBytes",
    "BackupProgressInMegaBytes",
    "CurrentBackupRateInMegaBytesPerSecond",
    "TotalProvisionedStorageInMegaBytes",
    "AvgResizeRateInMegaBytesPerSecond",
    "DataTransferProgressPercent",
    "CurrentRestoreRateInMegaBytesPerSecond",
    "CurrentRateInMegaBytesPerSecond",
    "EstimatedDiskUtilizationPercent",
];

/// Numeric members of those SAME shapes that the model targets at `Integer` or
/// `Long` instead, which carry no fractional part at all: the other half of the
/// rule, so a renderer that gave every number one would fail this suite.
///
/// Source: the same shapes as above — `ReservedNodeOffering$Duration`,
/// `ReservedNode$NodeCount`, `Snapshot$NumberOfNodes` and `Snapshot$Port`
/// (`Integer`); `Snapshot$EstimatedSecondsToCompletion`,
/// `RestoreStatus$ProgressInMegaBytes` and `RestoreStatus$ElapsedTimeInSeconds`
/// (`Long`).
const WHOLE_NUMBER_ELEMENTS: [&str; 7] = [
    "Duration",
    "NodeCount",
    "NumberOfNodes",
    "Port",
    "EstimatedSecondsToCompletion",
    "ProgressInMegaBytes",
    "ElapsedTimeInSeconds",
];

/// One row of the table below: the case name, the operation it reads, the
/// parameters it reads it with, and the fewest `double` elements the document it
/// answers must carry.
struct DocumentCase {
    case: &'static str,
    operation: &'static str,
    params: Vec<(&'static str, String)>,
    fewest_doubles: usize,
}

/// Assert the rule over one served document, answering how many `double`
/// elements it carried.
///
/// A double's text must parse as a number AND carry a fractional part; a
/// whole-number element's text must parse as an integer and carry none.
fn assert_rendered_as_modelled(case: &str, body: &str) -> usize {
    let mut doubles = 0usize;
    for element in DOUBLE_ELEMENTS {
        for text in all_text(body, element) {
            text.parse::<f64>().unwrap_or_else(|_| {
                panic!("case {case}: <{element}> must be a number, got {text:?}")
            });
            let fractional = text.split_once('.').map(|(_, digits)| digits);
            assert!(
                fractional.is_some_and(|digits| !digits.is_empty()),
                "case {case}: expected <{element}> rendered with a fractional part, got {text:?}"
            );
            doubles += 1;
        }
    }
    for element in WHOLE_NUMBER_ELEMENTS {
        for text in all_text(body, element) {
            assert!(
                !text.contains('.'),
                "case {case}: expected <{element}> rendered without a fractional part, got {text:?}"
            );
            text.parse::<i64>().unwrap_or_else(|_| {
                panic!("case {case}: <{element}> must be a whole number, got {text:?}")
            });
        }
    }
    doubles
}

#[tokio::test]
async fn every_double_of_a_served_document_carries_a_fractional_part() {
    let emulator = Emulator::new();

    // A reservation of the account's own, so the rule is asserted on a shape
    // built from a WRITE as well as on the published catalog.
    let offerings = catalog(&emulator).await;
    let bought = offering_of(
        "a one-year reservation",
        &offerings,
        "dc2.large",
        ONE_YEAR,
        "No Upfront",
    );
    let purchased = purchase(&emulator, &bought, Some(2), "a one-year reservation").await;
    assert_eq!(
        purchased.status, 200,
        "case buy a reservation: expected 200, got {} with body {}",
        purchased.status, purchased.body
    );
    let offering_id = one("a one-year reservation", &bought, "ReservedNodeOfferingId");

    // The rows differ along what the rule generalises over: how many doubles the
    // document carries (two, a record's three, the whole catalog's), whether
    // they sit in a top-level structure or nested inside a list, and whether the
    // document comes from a published catalog or from what the account holds.
    let rows = [
        DocumentCase {
            case: "the storage summary",
            operation: "DescribeStorage",
            params: vec![],
            fewest_doubles: 2,
        },
        DocumentCase {
            case: "one published offering",
            operation: "DescribeReservedNodeOfferings",
            params: vec![("ReservedNodeOfferingId", offering_id)],
            fewest_doubles: 3,
        },
        DocumentCase {
            case: "the whole published catalog",
            operation: "DescribeReservedNodeOfferings",
            params: vec![],
            fewest_doubles: 3 * 60,
        },
        DocumentCase {
            case: "the reservations the account holds",
            operation: "DescribeReservedNodes",
            params: vec![],
            fewest_doubles: 3,
        },
        DocumentCase {
            case: "the reservation the purchase reported",
            operation: "PurchaseReservedNodeOffering",
            params: vec![],
            fewest_doubles: 0,
        },
    ];

    for row in rows {
        // The purchase response is the one already in hand: re-buying would
        // reserve a second set of nodes.
        let answer = if row.operation == "PurchaseReservedNodeOffering" {
            &purchased
        } else {
            let params: Vec<(&str, &str)> = row
                .params
                .iter()
                .map(|(name, value)| (*name, value.as_str()))
                .collect();
            &emulator.call(row.operation, &params).await
        };
        assert_eq!(
            answer.status,
            200,
            "case {}: expected 200, got {} / {:?} with body {}",
            row.case,
            answer.status,
            answer.error_code(),
            answer.body
        );
        let doubles = assert_rendered_as_modelled(row.case, &answer.body);
        assert!(
            doubles >= row.fewest_doubles,
            "case {}: expected at least {} double element(s) asserted, got {doubles} in body {}",
            row.case,
            row.fewest_doubles,
            answer.body
        );
    }
}

/// One row of the rendering table: the payment option whose offering is read,
/// and the text each of its three prices must carry on the wire.
struct PriceCase {
    case: &'static str,
    offering_type: &'static str,
    /// `(element, the text it must carry)`.
    prices: [(&'static str, &'static str); 3],
}

#[tokio::test]
async fn a_whole_price_reads_zero_point_zero_and_a_fractional_one_keeps_its_digits() {
    let emulator = Emulator::new();

    let answer = emulator.call("DescribeReservedNodeOfferings", &[]).await;
    assert_eq!(
        answer.status, 200,
        "case the whole catalog: expected 200, got {} with body {}",
        answer.status, answer.body
    );
    let offerings = element_blocks(&answer.body, "ReservedNodeOffering");

    // Rows differ along what the rule generalises over: a value of nothing, a
    // value with fractional digits of its own, and a whole value that is not
    // zero — each as the TEXT the document must carry, written out here.
    let rows = [
        PriceCase {
            case: "a term paid for hourly",
            offering_type: "No Upfront",
            prices: [
                ("FixedPrice", "0.0"),
                ("UsagePrice", "0.0"),
                ("RecurringChargeAmount", "0.175"),
            ],
        },
        PriceCase {
            case: "a term paid for half upfront",
            offering_type: "Partial Upfront",
            prices: [
                ("FixedPrice", "766.5"),
                ("UsagePrice", "0.0"),
                ("RecurringChargeAmount", "0.088"),
            ],
        },
        PriceCase {
            case: "a term paid for in full upfront",
            offering_type: "All Upfront",
            prices: [
                ("FixedPrice", "1533.0"),
                ("UsagePrice", "0.0"),
                ("RecurringChargeAmount", "0.0"),
            ],
        },
    ];

    for row in rows {
        // One node type and one term across the three payment options, so the
        // only thing differing between the rows is the VALUE each price holds.
        let block = offering_of(
            row.case,
            &offerings,
            "dc2.large",
            ONE_YEAR,
            row.offering_type,
        );
        for (element, text) in row.prices {
            let served = one(row.case, &block, element);
            assert_eq!(
                served, text,
                "case {}: expected <{element}> {text:?}, got {served:?}",
                row.case
            );
        }
    }

    // The term in seconds is an `Integer` member of the same record, so it
    // carries no fractional part however the prices of that record are rendered.
    for block in &offerings {
        let duration = one("every offering", block, "Duration");
        assert!(
            !duration.contains('.'),
            "case every offering: expected <Duration> as a whole number, got {duration:?}"
        );
    }
}
