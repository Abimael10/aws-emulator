// SPDX-License-Identifier: CC0-1.0

//! The order a refusal LISTS the operating systems Patch Manager knows.
//!
//! RULE: a value outside an enumerated type is refused `ValidationException` whose clause
//! names the values that type admits, and the sequence inside the brackets is FIXED TEXT of
//! that message: for `OperatingSystem` the live service publishes one order, and because the
//! sequence belongs to the TYPE rather than to a member or an operation, every operation
//! carrying an `OperatingSystem` lists it identically. Only the rejected value comes from
//! the request.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body.
//!
//! The rows differ along what the rule generalises over: which operation carried the value,
//! where in its input the member sits, and which unmodelled value was sent — rather than the
//! single call a recorded unit happened to make.

use serde_json::{json, Value};

use crate::parameter_store::{call, error_code, error_message, router};

/// The order the live service lists `OperatingSystem`'s 15 values in — the bracketed
/// sequence of the refusal, in full.
const LISTED: &[&str] = &[
    "RASPBIAN",
    "WINDOWS",
    "AMAZON_LINUX_2022",
    "AMAZON_LINUX_2023",
    "UBUNTU",
    "ROCKY_LINUX",
    "REDHAT_ENTERPRISE_LINUX",
    "CENTOS",
    "AMAZON_LINUX_2",
    "MACOS",
    "ORACLE_LINUX",
    "SUSE",
    "ALMA_LINUX",
    "DEBIAN",
    "AMAZON_LINUX",
];

/// The 15 values the model declares for that type (`smithy_query(query="shape",
/// shape="OperatingSystem")`), so the listing above is checked to be a permutation of the
/// type itself rather than a list of its own.
const DECLARED: &[&str] = &[
    "WINDOWS",
    "AMAZON_LINUX",
    "AMAZON_LINUX_2",
    "AMAZON_LINUX_2022",
    "UBUNTU",
    "REDHAT_ENTERPRISE_LINUX",
    "SUSE",
    "CENTOS",
    "ORACLE_LINUX",
    "DEBIAN",
    "MACOS",
    "RASPBIAN",
    "ROCKY_LINUX",
    "ALMA_LINUX",
    "AMAZON_LINUX_2023",
];

/// The clause a refusal of `value` at `path` states, with the value set listed as the
/// service lists it.
fn expected_clause(value: &str, path: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy constraint: \
         Member must satisfy enum value set: [{}]",
        LISTED.join(", ")
    )
}

/// Values no model revision declares, each of a different shape: a plausible name, an empty
/// value, a lower-case spelling of a value that IS declared, and one carrying a separator.
const UNMODELLED: &[&str] = &["PLAN9", "", "windows", "AMAZON-LINUX-3"];

/// One row of the table: the operation that carried the value, the body it carried it in,
/// and the member path the clause names.
struct Row {
    case: &'static str,
    operation: &'static str,
    /// The request body carrying one unmodelled value at [`Row::path`].
    body: fn(&str) -> Value,
    path: &'static str,
}

/// RULE: every operation carrying the member lists the type the same way.
#[tokio::test]
async fn every_operation_carrying_an_operating_system_lists_it_alike() {
    assert_eq!(
        {
            let mut sorted = LISTED.to_vec();
            sorted.sort_unstable();
            sorted
        },
        {
            let mut sorted = DECLARED.to_vec();
            sorted.sort_unstable();
            sorted
        },
        "the listing must be a permutation of the type the model declares"
    );

    let router = router();
    // One row per operation of this model that carries an `OperatingSystem`: a read, a
    // create and a catalogue listing.
    let rows: Vec<Row> = vec![
        Row {
            case: "the read of a Region's default baseline",
            operation: "GetDefaultPatchBaseline",
            body: |value| json!({ "OperatingSystem": value }),
            path: "operatingSystem",
        },
        Row {
            case: "a create of a baseline",
            operation: "CreatePatchBaseline",
            body: |value| json!({ "Name": "Listing.Order.Baseline", "OperatingSystem": value }),
            path: "operatingSystem",
        },
        Row {
            case: "a listing of the patch properties",
            operation: "DescribePatchProperties",
            body: |value| json!({ "OperatingSystem": value, "Property": "PRODUCT" }),
            path: "operatingSystem",
        },
    ];

    for Row {
        case,
        operation,
        body,
        path,
    } in rows
    {
        for value in UNMODELLED {
            let case = format!("{case} with {value:?}");
            let (status, response) = call(&router, operation, &body(value)).await;
            assert_eq!(
                (status, error_code(&response).as_str()),
                (400, "ValidationException"),
                "case {case:?}: a value outside the type is refused, got {status} with {response}"
            );
            assert_eq!(
                error_message(&response),
                expected_clause(value, path),
                "case {case:?}: expected the published listing order, got {response}"
            );
        }
    }
}

/// RULE, stated on the one pair the derivation cannot settle: the published order puts
/// `SUSE` before `ALMA_LINUX`, the two values whose string hashes fall in one bucket.
#[tokio::test]
async fn the_two_values_sharing_a_hash_bucket_are_listed_as_published() {
    let router = router();
    let case = "an operating system outside the type";
    let (status, response) = call(
        &router,
        "GetDefaultPatchBaseline",
        &json!({ "OperatingSystem": "NOT_A_REAL_OS" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the refusal, got {status} with {response}"
    );
    let message = error_message(&response);
    let suse = message
        .find(", SUSE,")
        .unwrap_or_else(|| panic!("case {case:?}: expected SUSE in the listing, got {message:?}"));
    let alma = message.find(", ALMA_LINUX,").unwrap_or_else(|| {
        panic!("case {case:?}: expected ALMA_LINUX in the listing, got {message:?}")
    });
    assert!(
        suse < alma,
        "case {case:?}: the published order lists SUSE before ALMA_LINUX, got {message:?}"
    );
}
