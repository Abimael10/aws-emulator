// SPDX-License-Identifier: CC0-1.0

//! The available management network CIDR ranges, end to end over the awsJson1_1 wire.
//!
//! The rules under test, stated about the search range a request names and the BYOL
//! configuration this emulator persists rather than about any one recorded request:
//!
//! * `ListAvailableManagementCidrRanges` reports the `/16` management blocks that
//!   overlap the `ManagementCidrRangeConstraint` the request names — every block
//!   inside a constraint shorter than a `/16`, the single containing block for one
//!   longer — in ascending address order, each rendered as the `a.b.0.0/16` the
//!   model's `DedicatedTenancyManagementCidrRange` pattern admits.
//! * A block already configured as this account's management network CIDR range is
//!   in use, so it is not reported as available.
//! * A page holds at most `MaxResults` blocks (at most the modelled maximum of five
//!   when the request names none) and carries a `NextToken` only while an available
//!   block is left beyond it; resuming with that token continues strictly after the
//!   last block reported.
//! * Reporting changes nothing a later caller sees.
//!
//! The suite reuses `workspace_lifecycle`'s harness (`emulator_with_engine` + `call`),
//! so every assertion is about what a client would see: the request is serialized,
//! routed, validated, handled and serialized back.

use serde_json::{json, Value};

use crate::workspace_lifecycle::{call, emulator_with_engine};

/// The `ManagementCidrRanges` a response reports, as owned strings.
fn reported_ranges(body: &Value) -> Vec<String> {
    body["ManagementCidrRanges"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `value` is an `a.b.0.0/16`, which is the whole of what the model's
/// `DedicatedTenancyManagementCidrRange` pattern
/// (`smithy_query(query="shape",shape="DedicatedTenancyCidrRangeList")`) admits:
/// `^(^(…)\.(…)\.0\.0)(\/(16$))$`.
fn is_management_block(value: &str) -> bool {
    let Some((address, prefix)) = value.split_once('/') else {
        return false;
    };
    let octets: Vec<&str> = address.split('.').collect();
    prefix == "16"
        && octets.len() == 4
        && octets[2] == "0"
        && octets[3] == "0"
        && octets[..2]
            .iter()
            .all(|octet| octet.parse::<u8>().is_ok() && (*octet == "0" || !octet.starts_with('0')))
}

/// One row: the search range to list, the page size to ask for, and the blocks that
/// must come back.
struct ListCase {
    constraint: &'static str,
    max_results: Option<u8>,
    expected: Vec<&'static str>,
    expect_more: bool,
}

/// The listing table.
///
/// The rows differ along what the rule generalises over: the prefix length of the
/// search range (shorter than, equal to and longer than a `/16`), the address it
/// starts at, the page size asked for, and how many blocks the range holds. The
/// addresses and prefixes come from the model's `ManagementCidrRangeConstraint`
/// pattern (`smithy_query(query="operation",operation="ListAvailableManagementCidrRanges")`),
/// which admits four octets of 0..=255 and a prefix of 0..=32 — so the table carries
/// both published prefix bounds (`/0` and `/32`) and both address bounds (`0.0.0.0`
/// and `255.255.255.255`), and the refusal table below carries one value past each.
fn list_table() -> Vec<ListCase> {
    vec![
        // A constraint that IS a /16 selects exactly itself.
        ListCase {
            constraint: "10.0.0.0/16",
            max_results: None,
            expected: vec!["10.0.0.0/16"],
            expect_more: false,
        },
        // Shorter than a /16: every block inside it, and fewer than a page of them.
        ListCase {
            constraint: "10.0.0.0/14",
            max_results: None,
            expected: vec![
                "10.0.0.0/16",
                "10.1.0.0/16",
                "10.2.0.0/16",
                "10.3.0.0/16",
            ],
            expect_more: false,
        },
        // Shorter still: more blocks than a page, so the page is capped and a token
        // says so.
        ListCase {
            constraint: "10.0.0.0/8",
            max_results: None,
            expected: vec![
                "10.0.0.0/16",
                "10.1.0.0/16",
                "10.2.0.0/16",
                "10.3.0.0/16",
                "10.4.0.0/16",
            ],
            expect_more: true,
        },
        // An explicit page size below the cap, over a different address range.
        ListCase {
            constraint: "172.16.0.0/12",
            max_results: Some(2),
            expected: vec!["172.16.0.0/16", "172.17.0.0/16"],
            expect_more: true,
        },
        // The low published bound of `MaxResults` (range 1..5).
        ListCase {
            constraint: "192.168.0.0/15",
            max_results: Some(1),
            expected: vec!["192.168.0.0/16"],
            expect_more: true,
        },
        // The high published bound of `MaxResults`.
        ListCase {
            constraint: "198.18.0.0/13",
            max_results: Some(5),
            expected: vec![
                "198.16.0.0/16",
                "198.17.0.0/16",
                "198.18.0.0/16",
                "198.19.0.0/16",
                "198.20.0.0/16",
            ],
            expect_more: true,
        },
        // Longer than a /16, and not on a block boundary: the containing block.
        ListCase {
            constraint: "192.168.1.128/25",
            max_results: None,
            expected: vec!["192.168.0.0/16"],
            expect_more: false,
        },
        // The high published bound of both the address and the prefix.
        ListCase {
            constraint: "255.255.255.255/32",
            max_results: None,
            expected: vec!["255.255.0.0/16"],
            expect_more: false,
        },
        // The low published bound of both: a /0 search range starts at the first
        // block of the IPv4 space.
        ListCase {
            constraint: "0.0.0.0/0",
            max_results: Some(3),
            expected: vec!["0.0.0.0/16", "0.1.0.0/16", "0.2.0.0/16"],
            expect_more: true,
        },
        // A search range whose host bits are set still names the block span of its
        // network, not of the address.
        ListCase {
            constraint: "100.64.5.7/15",
            max_results: None,
            expected: vec!["100.64.0.0/16", "100.65.0.0/16"],
            expect_more: false,
        },
    ]
}

#[tokio::test]
async fn the_blocks_reported_are_the_ones_the_search_range_covers() {
    for case in list_table() {
        let (router, _engine) = emulator_with_engine();
        let ListCase {
            constraint,
            max_results,
            expected,
            expect_more,
        } = case;
        let label = format!("{constraint} max {max_results:?}");

        let mut request = json!({ "ManagementCidrRangeConstraint": constraint });
        if let Some(max_results) = max_results {
            request["MaxResults"] = json!(max_results);
        }
        let (status, body) = call(&router, "ListAvailableManagementCidrRanges", &request).await;
        assert_eq!(
            status, 200,
            "case {label}: expected HTTP 200, got {status} with body {body:?}"
        );
        let ranges = reported_ranges(&body);
        assert_eq!(
            ranges, expected,
            "case {label}: expected {expected:?}, got {ranges:?}"
        );
        for range in &ranges {
            assert!(
                is_management_block(range),
                "case {label}: {range} is not an a.b.0.0/16, got {ranges:?}"
            );
        }
        assert_eq!(
            body["NextToken"].as_str().is_some(),
            expect_more,
            "case {label}: expected NextToken present = {expect_more}, got {body:?}"
        );

        // Reporting changes nothing a later caller sees: the same request answers the
        // same page.
        let (second_status, second_body) =
            call(&router, "ListAvailableManagementCidrRanges", &request).await;
        assert_eq!(
            second_status, 200,
            "case {label}: the repeated read must answer 200, got {second_body:?}"
        );
        assert_eq!(
            reported_ranges(&second_body),
            ranges,
            "case {label}: expected the repeated read to report {ranges:?}, got {second_body:?}"
        );
    }
}

#[tokio::test]
async fn the_range_configured_for_the_account_is_no_longer_available() {
    // Each row: the range to configure through `ModifyAccount`, the search range to
    // list, and the blocks that must come back with the configured one withheld. The
    // rows differ in where the withheld block falls in the page — first, middle,
    // last — and in whether it is inside the search range at all.
    let cases: Vec<(&str, &str, Option<u8>, Vec<&str>)> = vec![
        (
            "10.1.0.0/16",
            "10.0.0.0/8",
            Some(3),
            vec!["10.0.0.0/16", "10.2.0.0/16", "10.3.0.0/16"],
        ),
        (
            "10.0.0.0/16",
            "10.0.0.0/14",
            None,
            vec!["10.1.0.0/16", "10.2.0.0/16", "10.3.0.0/16"],
        ),
        ("10.3.0.0/16", "10.0.0.0/14", None, vec![
            "10.0.0.0/16",
            "10.1.0.0/16",
            "10.2.0.0/16",
        ]),
        // The only block the search range holds is the configured one, so nothing is
        // available in it.
        ("172.16.0.0/16", "172.16.0.0/16", None, vec![]),
        // A configured range outside the search range withholds nothing.
        (
            "192.168.0.0/16",
            "10.0.0.0/15",
            None,
            vec!["10.0.0.0/16", "10.1.0.0/16"],
        ),
    ];

    for (configured, constraint, max_results, expected) in cases {
        let (router, _engine) = emulator_with_engine();
        let label = format!("configured {configured} searching {constraint}");

        let (status, body) = call(
            &router,
            "ModifyAccount",
            &json!({
                "DedicatedTenancySupport": "ENABLED",
                "DedicatedTenancyManagementCidrRange": configured,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label}: ModifyAccount must answer 200, body {body:?}"
        );

        let mut request = json!({ "ManagementCidrRangeConstraint": constraint });
        if let Some(max_results) = max_results {
            request["MaxResults"] = json!(max_results);
        }
        let (status, body) = call(&router, "ListAvailableManagementCidrRanges", &request).await;
        assert_eq!(
            status, 200,
            "case {label}: expected HTTP 200, got {status} with body {body:?}"
        );
        let ranges = reported_ranges(&body);
        assert_eq!(
            ranges, expected,
            "case {label}: expected {expected:?}, got {ranges:?}"
        );
        assert!(
            !ranges.iter().any(|range| range == configured),
            "case {label}: the configured range must not be reported, got {ranges:?}"
        );
    }
}

#[tokio::test]
async fn following_the_token_walks_the_search_range_once_in_order() {
    // Each row: a search range, the page size to walk it with, and every block the
    // walk must visit. The rows differ in page size and in how the total divides by
    // it — exactly, with a remainder, and in a single page.
    let cases: Vec<(&str, u8, Vec<&str>)> = vec![
        (
            "10.0.0.0/14",
            2,
            vec![
                "10.0.0.0/16",
                "10.1.0.0/16",
                "10.2.0.0/16",
                "10.3.0.0/16",
            ],
        ),
        (
            "172.16.0.0/14",
            3,
            vec![
                "172.16.0.0/16",
                "172.17.0.0/16",
                "172.18.0.0/16",
                "172.19.0.0/16",
            ],
        ),
        ("203.0.0.0/15", 5, vec!["203.0.0.0/16", "203.1.0.0/16"]),
    ];

    for (constraint, page_size, expected) in cases {
        let (router, _engine) = emulator_with_engine();
        let label = format!("{constraint} in pages of {page_size}");

        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One more page than the walk can need, so a token that never settles fails
        // the case instead of looping.
        let page_budget = expected.len() + 2;
        for page in 0..page_budget {
            let mut request = json!({
                "ManagementCidrRangeConstraint": constraint,
                "MaxResults": page_size,
            });
            if let Some(token) = token.as_deref() {
                request["NextToken"] = json!(token);
            }
            let (status, body) = call(&router, "ListAvailableManagementCidrRanges", &request).await;
            assert_eq!(
                status, 200,
                "case {label}: page {page} expected HTTP 200, got {status} with body {body:?}"
            );
            let ranges = reported_ranges(&body);
            assert!(
                ranges.len() <= usize::from(page_size),
                "case {label}: page {page} holds more than {page_size} blocks: {ranges:?}"
            );
            seen.extend(ranges);
            token = body["NextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }

        assert!(
            token.is_none(),
            "case {label}: the walk did not finish within {page_budget} pages, saw {seen:?}"
        );
        assert_eq!(
            seen, expected,
            "case {label}: expected the walk to visit {expected:?}, got {seen:?}"
        );
    }
}

#[tokio::test]
async fn requests_that_cannot_be_served_are_refused() {
    // Each row: the request, why it cannot be served, and the error code the refusal
    // carries — the handler's own `InvalidParameterValuesException` for a value it
    // judges, boundary `ValidationException` for one the member's own constraint
    // rejects. The constraint values are one past each published bound of the
    // `ManagementCidrRangeConstraint` pattern, and the `MaxResults` values one past
    // each published bound of `ManagementCidrRangeMaxResults` (range 1..5).
    let cases: Vec<(Value, &str, &str)> = vec![
        (
            json!({ "ManagementCidrRangeConstraint": "10.0.0.0/33" }),
            "33 is past the prefix length the pattern admits",
            "ValidationException",
        ),
        (
            json!({ "ManagementCidrRangeConstraint": "256.0.0.0/16" }),
            "256 is past the octet range the pattern admits",
            "ValidationException",
        ),
        (
            json!({ "ManagementCidrRangeConstraint": "10.0.0.0" }),
            "a bare address is not an IP address range in CIDR notation",
            "ValidationException",
        ),
        (
            json!({}),
            "ManagementCidrRangeConstraint is required",
            "ValidationException",
        ),
        (
            json!({ "ManagementCidrRangeConstraint": "10.0.0.0/8", "MaxResults": 0 }),
            "0 is below the MaxResults range",
            "ValidationException",
        ),
        (
            json!({ "ManagementCidrRangeConstraint": "10.0.0.0/8", "MaxResults": 6 }),
            "6 is past the MaxResults range",
            "ValidationException",
        ),
        (
            json!({ "ManagementCidrRangeConstraint": "10.0.0.0/8", "NextToken": "not-a-token" }),
            "the token was not minted by this service",
            "InvalidParameterValuesException",
        ),
    ];

    for (request, why, expected_code) in cases {
        let (router, _engine) = emulator_with_engine();

        let (status, body) = call(&router, "ListAvailableManagementCidrRanges", &request).await;
        assert_eq!(
            status, 400,
            "case {request:?} ({why}): expected HTTP 400, got {status} with body {body:?}"
        );
        let code = body["__type"].as_str().unwrap_or_default();
        assert!(
            code.ends_with(expected_code),
            "case {request:?} ({why}): expected {expected_code}, got {body:?}"
        );
    }
}

#[tokio::test]
async fn a_token_past_the_end_of_the_search_range_reports_no_blocks() {
    let (router, _engine) = emulator_with_engine();

    let request = json!({
        "ManagementCidrRangeConstraint": "10.0.0.0/15",
        "MaxResults": 1,
    });
    let (status, body) = call(&router, "ListAvailableManagementCidrRanges", &request).await;
    assert_eq!(
        status, 200,
        "case first page: expected HTTP 200, got {status} with body {body:?}"
    );
    let token = body["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case first page: expected a NextToken, got {body:?}"))
        .to_string();

    let last_page = json!({
        "ManagementCidrRangeConstraint": "10.0.0.0/15",
        "MaxResults": 1,
        "NextToken": token,
    });
    let (status, body) = call(&router, "ListAvailableManagementCidrRanges", &last_page).await;
    assert_eq!(
        status, 200,
        "case last page: expected HTTP 200, got {status} with body {body:?}"
    );
    assert_eq!(
        reported_ranges(&body),
        vec!["10.1.0.0/16".to_string()],
        "case last page: expected the second and last block, got {body:?}"
    );
    assert!(
        body["NextToken"].as_str().is_none(),
        "case last page: expected no NextToken, got {body:?}"
    );

    // The same token on a search range that holds nothing beyond it reports an empty
    // list rather than wrapping around.
    let past_the_end = json!({
        "ManagementCidrRangeConstraint": "10.1.0.0/16",
        "NextToken": token_for_block(0x0a01),
    });
    let (status, body) = call(&router, "ListAvailableManagementCidrRanges", &past_the_end).await;
    assert_eq!(
        status, 200,
        "case past the end: expected HTTP 200, got {status} with body {body:?}"
    );
    assert_eq!(
        reported_ranges(&body),
        Vec::<String>::new(),
        "case past the end: expected no blocks, got {body:?}"
    );
    assert!(
        body["NextToken"].as_str().is_none(),
        "case past the end: expected no NextToken, got {body:?}"
    );
}

/// The `NextToken` that resumes after management block `index`, in the one encoding
/// every paginated reader in this service shares
/// (`service_com_amazonaws_workspaces_workspacesservice::state::encode_page_token`).
fn token_for_block(index: u64) -> String {
    service_com_amazonaws_workspaces_workspacesservice::state::encode_page_token(index)
}
