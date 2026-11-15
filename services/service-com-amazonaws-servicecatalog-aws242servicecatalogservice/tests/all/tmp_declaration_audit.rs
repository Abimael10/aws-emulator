// SPDX-License-Identifier: CC0-1.0

//! The scaffold census this crate declares, against what each operation answers.
//!
//! A scaffolded operation carries one marker line in its handler — the machine-readable
//! record of which operations are still stubs — and answers HTTP 501 `NotImplemented`. The
//! two halves have to agree: a marker left on an operation that was implemented
//! under-reports the work done, and a marker deleted from one that still answers 501
//! over-reports it. Neither is visible from the handler alone, so this suite reads the
//! census out of `src/ops/` and drives the operation it names over the wire.
//!
//! The rows differ along which side of the census the operation sits on, so the suite
//! stays correct as operations are implemented: what each row asserts is DERIVED from the
//! marker, never from a list of operation names this file keeps.

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use crate::harness::{call, emulator, error_code};

/// The line a scaffolded handler carries, as the census greps for it. Written here as a
/// value rather than as a comment so this suite is not itself a census entry.
const SCAFFOLD_MARKER: &str = "//! SCAFFOLDED, NOT IMPLEMENTED:";

/// Whether the handler of the operation whose module is `module` still carries the
/// scaffold marker.
fn is_scaffolded(module: &str) -> bool {
    let handler = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("ops")
        .join(format!("{module}.rs"));
    let source = fs::read_to_string(&handler)
        .unwrap_or_else(|error| panic!("read {}: {error}", handler.display()));
    source
        .lines()
        .any(|line| line.starts_with(SCAFFOLD_MARKER))
}

/// One row: the wire operation, the handler module that carries its census entry, and a
/// request that reaches the handler (every modelled constraint satisfied, so what comes
/// back is the operation's own answer rather than a validation refusal).
struct CensusCase {
    operation: &'static str,
    module: &'static str,
    request: fn() -> Value,
}

/// Rows on both sides of the census, each a request whose modelled constraints hold.
const CASES: &[CensusCase] = &[
    CensusCase {
        operation: "GetProvisionedProductOutputs",
        module: "get_provisioned_product_outputs",
        request: || json!({"ProvisionedProductName": "no-such-provisioned-product"}),
    },
    CensusCase {
        operation: "ListPortfolios",
        module: "list_portfolios",
        request: || json!({}),
    },
    CensusCase {
        operation: "ListTagOptions",
        module: "list_tag_options",
        request: || json!({}),
    },
    CensusCase {
        operation: "GetAwsOrganizationsAccessStatus",
        module: "get_aws_organizations_access_status",
        request: || json!({}),
    },
    CensusCase {
        operation: "DescribeRecord",
        module: "describe_record",
        request: || json!({"Id": "rec-000000000000"}),
    },
    CensusCase {
        operation: "SearchProducts",
        module: "search_products",
        request: || json!({}),
    },
];

/// An operation answers 501 `NotImplemented` exactly while its handler carries the
/// scaffold marker, and answers its own modelled result once that marker is gone.
#[tokio::test]
async fn the_scaffold_census_agrees_with_what_each_operation_answers() {
    let router = emulator();
    for case in CASES {
        let scaffolded = is_scaffolded(case.module);
        let (status, body) = call(&router, case.operation, &(case.request)()).await;
        match scaffolded {
            true => {
                assert_eq!(
                    status, 501,
                    "case {}: its handler still carries the census marker, so it must answer \
                     501, got {status} with {body}",
                    case.operation
                );
                assert_eq!(
                    error_code(&body),
                    "NotImplemented",
                    "case {}: a stub's refusal is NotImplemented, got {body}",
                    case.operation
                );
            }
            false => assert_ne!(
                status, 501,
                "case {}: its handler carries no census marker, so it must answer the \
                 operation rather than 501, got {body}",
                case.operation
            ),
        }
    }
}

/// The operation this crate's own `GetProvisionedProductOutputs` fix implemented is off
/// the census, and the refusal it answers for a provisioned product nothing stored carries
/// is the modelled one rather than a stub's.
#[tokio::test]
async fn the_implemented_outputs_read_is_off_the_census() {
    assert!(
        !is_scaffolded("get_provisioned_product_outputs"),
        "GetProvisionedProductOutputs is implemented, so its handler carries no census marker"
    );
    let router = emulator();
    let (status, body) = call(
        &router,
        "GetProvisionedProductOutputs",
        &json!({"ProvisionedProductId": "pp-000000000000"}),
    )
    .await;
    assert_eq!(
        status, 400,
        "an id nothing stored carries is the modelled not-found, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceNotFoundException",
        "an id nothing stored carries is the modelled not-found, got {body}"
    );
}
