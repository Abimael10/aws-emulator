// SPDX-License-Identifier: CC0-1.0

//! That the integrated-service inventory ENUMERATES each `ServiceCode` AWS registers,
//! under the display name AWS registers for it, and that a client discovers it there
//! however it pages the listing.
//!
//! THE RULE, stated over the inventory rather than over one entry: `ListServices`
//! reports one `ServiceInfo` per `ServiceCode` AWS registers as integrated with Service
//! Quotas, ordered by `ServiceCode`, and the `ServiceName` of an entry is the display
//! name AWS registers for THAT code, reproduced as it is registered — with or without
//! an `Amazon`/`AWS` prefix, as one unspaced word, as a bare initialism, or with a
//! short name in parentheses (`smithy_query(query="shape", shape="ServiceInfo")`:
//! `ServiceInfo{ServiceCode @length 1..63 @pattern ^[a-zA-Z][a-zA-Z0-9-]{1,63}$,
//! ServiceName}`; `smithy_query(query="operation", operation="ListServices")`: output
//! `{NextToken, Services: ServiceInfoListDefinition}`, paginated on
//! `NextToken`/`MaxResults` @range 1..100).
//!
//! What this suite owns, and what it does NOT: `integrated_service_display_names.rs`
//! owns the SHAPE of a display name, that a name is the code's own rather than a
//! sibling's, and that every quota row repeats it. This suite owns COVERAGE and
//! DISCOVERY — a registered code the catalog omits is unreachable, and a code a client
//! only ever sees at a page boundary is only discoverable if paging yields the one
//! ordered listing. So the rows here are codes, and the assertions are about where in
//! the paged listing each one is found.
//!
//! The harness (`client`, `emulator`, `service_inventory`, `walk_pages`) is the one
//! `quota_workflow.rs` owns.

use serde_json::json;

use service_com_amazonaws_servicequotas_servicequotasv20190624::state::MAX_PAGE_SIZE;

use crate::quota_workflow::{client, emulator, service_inventory, walk_pages};

/// One row of AWS's integrated-service inventory.
struct Registered {
    code: &'static str,
    name: &'static str,
    /// What makes this row a different case of the rule from the others.
    form: &'static str,
}

/// The rows this suite pins.
///
/// SOURCE: AWS's own integrated-service inventory — the `ServiceInfo{ServiceCode,
/// ServiceName}` pairs `aws service-quotas list-services` reports for these codes (the
/// model enumerates no catalog: `smithy_query(query="shape", shape="ServiceInfo")`
/// carries the two members and the `ServiceCode` constraints, and nothing more).
///
/// The rows span the forms a registered display name takes rather than repeating one
/// case: a prefixed product name, a product name with NO prefix, a name spelled as one
/// unspaced word, a bare initialism, a single word, a name carrying a prefixed
/// abbreviation in parentheses, a name carrying the product's short name in
/// parentheses, and both halves of a hyphenated family pair.
const REGISTERED: &[Registered] = &[
    Registered {
        code: "deadline",
        name: "AWS Deadline Cloud",
        form: "a prefixed product name",
    },
    Registered {
        code: "drs",
        name: "ElasticDisasterRecovery",
        form: "a display name spelled as one unspaced word, with no prefix",
    },
    Registered {
        code: "ec2-ipam",
        name: "IPAM",
        form: "a display name that is the feature's initialism alone",
    },
    Registered {
        code: "ec2fastlaunch",
        name: "EC2 Fast Launch",
        form: "a product name the inventory carries no prefix on",
    },
    Registered {
        code: "eks",
        name: "Amazon Elastic Kubernetes Service (Amazon EKS)",
        form: "one half of a hyphenated family pair",
    },
    Registered {
        code: "eks-mcp",
        name: "Amazon EKS MCP",
        form: "the other half of that pair, named for its own plane",
    },
    Registered {
        code: "elemental-inference",
        name: "AWS Elemental Inference",
        form: "a hyphenated code under the AWS prefix",
    },
    Registered {
        code: "finspace",
        name: "Amazon FinSpace",
        form: "a single-word product name",
    },
    Registered {
        code: "fis",
        name: "AWS Fault Injection Service (AWS FIS)",
        form: "a name carrying the prefixed abbreviation in parentheses",
    },
    Registered {
        code: "geo",
        name: "Amazon Location Service (Amazon Location)",
        form: "a name carrying the product's short name in parentheses",
    },
];

/// The page sizes a client may walk the inventory with: the smallest, the largest the
/// model's `MaxResults` @range 1..100 admits, and one in between.
const PAGE_SIZES: &[i64] = &[1, 7, MAX_PAGE_SIZE as i64];

/// Every `(ServiceCode, ServiceName)` pair of a `Services` list, in reported order.
fn pairs(services: &[serde_json::Value]) -> Vec<(String, String)> {
    services
        .iter()
        .map(|service| {
            (
                service["ServiceCode"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case ListServices: an entry without a code"))
                    .to_string(),
                service["ServiceName"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case ListServices: an entry without a name"))
                    .to_string(),
            )
        })
        .collect()
}

/// Each registered code is enumerated exactly once, under the display name AWS
/// registers for it.
#[tokio::test]
async fn a_registered_code_is_enumerated_under_its_registered_display_name() {
    let engine = emulator();
    let client = client(&engine, "us-east-1");
    let inventory = pairs(&service_inventory(&client).await);

    for row in REGISTERED {
        let case = format!("{} ({})", row.code, row.form);
        let found: Vec<&str> = inventory
            .iter()
            .filter(|(code, _)| code == row.code)
            .map(|(_, name)| name.as_str())
            .collect();
        assert_eq!(
            found.len(),
            1,
            "case {case}: expected the inventory to enumerate this code once, got {found:?} in \
             {} entries",
            inventory.len()
        );
        assert_eq!(
            found[0], row.name,
            "case {case}: expected the display name AWS registers for this code, got {:?}",
            found[0]
        );
    }
}

/// A registered code is discoverable however the client pages: every page size yields
/// the one ordered listing, and each row is found in it at the same place the unasked
/// listing holds it, never split away at a page boundary.
#[tokio::test]
async fn a_registered_code_is_discoverable_at_every_page_size() {
    let engine = emulator();
    let client = client(&engine, "eu-west-1");

    let unasked = pairs(&service_inventory(&client).await);
    for row in REGISTERED {
        assert!(
            unasked.iter().any(|(code, _)| code == row.code),
            "case {} ({}): the unasked listing must hold this code",
            row.code,
            row.form
        );
    }

    for size in PAGE_SIZES {
        let walked = pairs(
            &walk_pages(
                &client,
                "ListServices",
                &json!({}),
                "Services",
                Some(*size),
            )
            .await,
        );

        // Two independently obtained listings of the same catalog: the unasked page and
        // the token-chained walk. Each row must be at one place in both of them, so a
        // page boundary neither drops nor repeats an entry.
        for row in REGISTERED {
            let walked_at: Vec<usize> = walked
                .iter()
                .enumerate()
                .filter(|(_, (code, _))| code == row.code)
                .map(|(at, _)| at)
                .collect();
            let unasked_at: Vec<usize> = unasked
                .iter()
                .enumerate()
                .filter(|(_, (code, _))| code == row.code)
                .map(|(at, _)| at)
                .collect();
            assert_eq!(
                walked_at, unasked_at,
                "case MaxResults={size} {} ({}): expected the entry at the one position the \
                 unasked listing holds it at, got {walked_at:?}",
                row.code, row.form
            );
        }

        assert_eq!(
            walked,
            unasked,
            "case MaxResults={size}: a paged walk must yield the one inventory, got {} entries of \
             {}",
            walked.len(),
            unasked.len()
        );
        for pair in walked.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "case MaxResults={size}: the walk stays in ServiceCode order, got {:?} before \
                 {:?}",
                pair[0].0,
                pair[1].0
            );
        }
    }
}
