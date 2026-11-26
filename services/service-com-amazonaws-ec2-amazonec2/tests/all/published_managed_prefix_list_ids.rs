// SPDX-License-Identifier: CC0-1.0

//! The ids of the prefix lists Amazon Web Services MANAGES, driven over the wire this service
//! speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML;
//! README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: a prefix list AWS manages carries the id AWS publishes for it. The id
//! is a fact of the list, kept for the life of the list and named in the documentation of each
//! Region, which is what lets a security group rule or a route be written against `pl-…`
//! directly — so a client that resolves a managed list by name and then references the id it
//! was given must be given the id the real Region has.
//!
//! The rows span the class: two services' lists in two Regions, which are four different
//! published ids, read back through BOTH reports that carry a managed list — the legacy
//! `DescribePrefixLists` and `DescribeManagedPrefixLists` — so neither report can answer a
//! different id for one list. A list whose published id this emulator does not hold is a row
//! too: it keeps the derived id, which is the same form (`pl-` and eight hexadecimal digits)
//! and is stable across reads, so no caller is handed an id that changes under it.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::call;
use super::wire;

/// The ids and names of one Region's managed prefix lists, as one report answered them.
async fn reported_lists(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    set: &str,
) -> Vec<(String, String)> {
    let (status, body) = call(router, region, action, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    let document = wire::document(case, &body);
    let root = document.root_element();
    wire::set_items(root, set)
        .into_iter()
        .map(|item| {
            (
                wire::child_text(item, "prefixListName").unwrap_or_default(),
                wire::child_text(item, "prefixListId").unwrap_or_default(),
            )
        })
        .collect()
}

/// Whether `id` is in the form a managed prefix list's id takes: `pl-` and eight hexadecimal
/// digits.
fn is_prefix_list_id(id: &str) -> bool {
    wire::is_hex_id("pl", id, 8)
}

/// Rule: a managed prefix list carries the id AWS publishes for it, in both reports.
#[tokio::test]
async fn a_managed_prefix_list_carries_the_id_aws_publishes() {
    let (router, _engine) = super::ec2_query::emulator();

    // (the Region, the list's name there, the id AWS publishes for it)
    let rows: [(&str, &str, &str); 4] = [
        ("us-west-2", "com.amazonaws.us-west-2.s3", "pl-68a54001"),
        (
            "us-west-2",
            "com.amazonaws.us-west-2.dynamodb",
            "pl-00a54069",
        ),
        ("us-east-1", "com.amazonaws.us-east-1.s3", "pl-63a5400a"),
        (
            "us-east-1",
            "com.amazonaws.us-east-1.dynamodb",
            "pl-02cd2c6b",
        ),
    ];

    for (region, prefix_list_name, published) in rows {
        for (action, set) in [
            ("DescribePrefixLists", "prefixListSet"),
            ("DescribeManagedPrefixLists", "prefixListSet"),
        ] {
            let case = format!("{region}/{prefix_list_name}/{action}");
            let reported = reported_lists(&case, &router, region, action, set).await;
            let found = reported
                .iter()
                .find(|(name, _)| name == prefix_list_name)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {prefix_list_name} must be reported, got {reported:?}")
                });
            assert_eq!(
                found.1, published,
                "case {case:?}: expected the published id {published:?}, got {:?}",
                found.1
            );
        }
    }

    // A list this emulator holds no published id for keeps the derived one: the same form, and
    // the same value on a second read, so an id a caller references never changes under it.
    let case = "a list with no published id";
    let first = reported_lists(case, &router, "us-west-2", "DescribeManagedPrefixLists", "prefixListSet")
        .await;
    let second = reported_lists(case, &router, "us-west-2", "DescribeManagedPrefixLists", "prefixListSet")
        .await;
    assert_eq!(
        first, second,
        "case {case:?}: two reads of the catalog must answer the same ids"
    );
    let derived = first
        .iter()
        .find(|(name, _)| name == "com.amazonaws.us-west-2.s3express")
        .unwrap_or_else(|| panic!("case {case:?}: the catalog must carry it, got {first:?}"));
    assert!(
        is_prefix_list_id(&derived.1),
        "case {case:?}: expected an id in the published form, got {:?}",
        derived.1
    );
    for (name, id) in &first {
        assert!(
            is_prefix_list_id(id),
            "case {case:?}: every managed list's id is in that one form, got {id:?} for {name}"
        );
    }
}
