// SPDX-License-Identifier: CC0-1.0

//! How this service STATES the admissible set of an enumerated type when the boundary
//! refuses a value outside it, driven end-to-end over the same router the emulator serves
//! with.
//!
//! RULE: a value outside an enumerated type is refused with the counted constraint envelope
//! over one clause naming the member, echoing the value the request sent and stating the
//! admissible set — and the set is listed in the order the front door's own TABLE of those
//! values is walked (each value's bucket ascending), not in the order the model declares
//! them. The order is computed from the values themselves, so it belongs to the enumerated
//! TYPE: one type reads the same way whatever order a schema presents its values in, and
//! whatever value a caller sent.
//!
//! The rows differ along what the rule generalises over: the rejected VALUE (a name of no
//! enum at all, a declared value in the wrong case, a declared value with a suffix, the empty
//! string, a long value, a value carrying the separator the values are spelled with) and
//! whether the rest of the request would have been accepted or refused on its own — the
//! enumerated member is decided at the boundary either way.
//!
//! `OutputFormat`'s declared values come from
//! `smithy_query(query="shape", shape="OutputFormat")` and are checked here against the
//! model-derived `ops::types::OUTPUT_FORMAT_VALUES`, so the set a refusal states cannot drift
//! from the type the model declares.

use serde_json::{json, Value};
use service_com_amazonaws_inspectorscan_inspectorscan::ops::types::OUTPUT_FORMAT_VALUES;
use std::collections::BTreeSet;

use super::scan_sbom::{bom, component, emulator, post_scan};

/// The error code a constraint refusal of this service carries.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The set `OutputFormat`'s refusal states, in the order it states them.
///
/// `OutputFormat` is declared `CYCLONE_DX_1_5`, `INSPECTOR`, `INSPECTOR_ALT`
/// (`smithy_query(query="shape", shape="OutputFormat")`); the three fall in distinct buckets
/// of the table the admissible values are held in, and the buckets rise in this order, which
/// is not the declaration's.
const OUTPUT_FORMAT_SET: &str = "INSPECTOR_ALT, CYCLONE_DX_1_5, INSPECTOR";

/// The whole message a refusal of `outputFormat` carries: the counted envelope over the one
/// clause, with the value the request sent echoed back.
fn expected_message(rejected: &str) -> String {
    format!(
        "1 validation error detected: Value '{rejected}' at 'outputFormat' failed to satisfy \
         constraint: Member must satisfy enum value set: [{OUTPUT_FORMAT_SET}]"
    )
}

/// A minimal valid CycloneDX 1.5 document carrying one component.
fn valid_sbom() -> Value {
    bom(vec![component(
        "ref-lodash",
        "lodash",
        "4.17.20",
        "pkg:npm/lodash@4.17.20",
    )])
}

/// One row: the `outputFormat` the request carried, the `sbom` beside it, and what the clause
/// must echo.
struct Row {
    case: &'static str,
    requested: &'static str,
    /// Whether the rest of the request would have been accepted on its own.
    sbom_is_valid: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a name of no enum at all",
        requested: "NOT_A_REAL_OUTPUT_FORMAT",
        sbom_is_valid: true,
    },
    Row {
        case: "a declared value in the wrong case",
        requested: "inspector",
        sbom_is_valid: true,
    },
    Row {
        case: "a declared value with a suffix",
        requested: "INSPECTOR_ALT_2",
        sbom_is_valid: true,
    },
    Row {
        case: "the empty string",
        requested: "",
        sbom_is_valid: true,
    },
    Row {
        case: "a value carrying the separator the values are spelled with",
        requested: "CYCLONE_DX_1_6",
        sbom_is_valid: true,
    },
    Row {
        case: "a long value",
        requested: "INSPECTOR_ALTERNATIVE_REPORT_FORMAT_VERSION_TWO",
        sbom_is_valid: true,
    },
    Row {
        // The member is decided at the boundary, before the document is read, so a request
        // whose `sbom` is ALSO defective is still refused for the enumerated member.
        case: "a bad format beside a document that does not parse",
        requested: "NOT_A_REAL_OUTPUT_FORMAT",
        sbom_is_valid: false,
    },
];

#[tokio::test]
async fn a_value_outside_the_output_format_enum_states_the_set_in_the_tables_order() {
    let (_engine, router) = emulator();
    for row in ROWS {
        let sbom = if row.sbom_is_valid {
            valid_sbom()
        } else {
            json!("this-is-not-a-cyclonedx-sbom-just-a-string")
        };
        let (status, response) = post_scan(
            &router,
            &json!({ "sbom": sbom, "outputFormat": row.requested }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status}: {response}",
            row.case
        );
        assert_eq!(
            response["__type"]
                .as_str()
                .map(|named| named.split('#').next_back().unwrap_or(named)),
            Some(VALIDATION_EXCEPTION),
            "case {:?}: expected the {VALIDATION_EXCEPTION} error code, got {response}",
            row.case
        );
        assert_eq!(
            response["message"].as_str(),
            Some(expected_message(row.requested).as_str()),
            "case {:?}: expected the constraint sentence with the set listed \
             [{OUTPUT_FORMAT_SET}], got {response}",
            row.case
        );
    }
}

/// The set a refusal states is exactly the type the model declares — nothing added and
/// nothing left out — so the listing above cannot drift from `OutputFormat`.
#[test]
fn the_stated_set_is_the_declared_set() {
    let stated: BTreeSet<&str> = OUTPUT_FORMAT_SET.split(", ").collect();
    let declared: BTreeSet<&str> = OUTPUT_FORMAT_VALUES.iter().copied().collect();
    assert_eq!(
        stated, declared,
        "the stated set must be the model's declared set, got stated {stated:?} and declared \
         {declared:?}"
    );
}

/// The order belongs to the enumerated TYPE, not to the order a schema presented its values
/// in: the same set presented declared, reversed and rotated is stated the same way.
#[test]
fn the_order_is_the_types_own_whatever_order_the_values_are_presented_in() {
    let declared: Vec<String> = OUTPUT_FORMAT_VALUES
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    let reversed: Vec<String> = declared.iter().rev().cloned().collect();
    let rotated: Vec<String> = declared[1..]
        .iter()
        .chain(declared[..1].iter())
        .cloned()
        .collect();
    for (case, presented) in [
        ("as the model declares them", &declared),
        ("reversed", &reversed),
        ("rotated", &rotated),
    ] {
        let listed =
            service_common::validation_message::hashed_enum_order(presented).join(", ");
        assert_eq!(
            listed, OUTPUT_FORMAT_SET,
            "case {case:?}: expected the set listed [{OUTPUT_FORMAT_SET}], got [{listed}]"
        );
    }
}
