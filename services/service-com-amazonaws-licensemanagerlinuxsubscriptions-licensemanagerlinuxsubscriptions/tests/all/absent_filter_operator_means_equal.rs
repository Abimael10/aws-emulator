// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for ONE rule of this service's `Filters`-driven lists, driven over the
//! real wire path: a `Filter` carrying no `Operator` asks for the `Equal` comparison, and
//! BOTH places a filter is read mean that same comparison by it.
//!
//! The two places are the two things a `Filters` request does, so each test below drives
//! one of them, through both operations that take a `Filters` selection
//! (`ListLinuxSubscriptionInstances` and `ListLinuxSubscriptions`):
//!
//! * the SELECTION: which elements the list reports — an absent `Operator` reports exactly
//!   what the `Equal` spelling of the same filter reports, and neither the `Contains` nor
//!   the `NotEqual` comparison of those values (the expected sets below are chosen so a
//!   default of either would report a different list);
//! * the LIST IDENTITY a pagination cursor is minted against — a cursor handed out by a
//!   page of the absent-`Operator` spelling continues the walk when it is sent back under
//!   the `Equal` spelling, and the other way round, because the two spell ONE selection.
//!
//! The rows span what the rule generalises over: the member the filter addresses (a scalar
//! member, the list member `ProductCode`, a member every record leaves unset), the number
//! of values compared against, a selection of everything down to a selection of nothing,
//! and both lists' own `Filter$Name` sets.

use serde_json::{json, Value};

use crate::harness::{
    call, enable_discovery, harness, seed_discovered_instance, Harness, Reply,
};
use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::state::DiscoveredInstanceRecord;

/// `POST /subscription/ListLinuxSubscriptionInstances` (`OPERATIONS.md`).
const INSTANCES_PATH: &str = "/subscription/ListLinuxSubscriptionInstances";
/// `POST /subscription/ListLinuxSubscriptions` (`OPERATIONS.md`).
const SUBSCRIPTIONS_PATH: &str = "/subscription/ListLinuxSubscriptions";

/// One subscription type per fixture name, from the model's documentation for
/// `Subscription$Type` ("subscription-included with Amazon EC2, Bring Your Own
/// Subscription model (BYOS), or from the Amazon Web Services Marketplace"), so every
/// aggregated subscription of the fixture has a distinct `Name`.
const TYPE_INCLUDED: &str = "Subscription-included (Amazon EC2)";
const TYPE_BYOS: &str = "Bring Your Own Subscription model (BYOS)";
const TYPE_MARKETPLACE: &str = "Amazon Web Services Marketplace";

/// One discovered instance of the fixture: instance id, account, AMI, instance type,
/// status, Region, usage operation, product codes, subscription name, subscription type.
type Row = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static str,
);

/// The discovered inventory every case below selects over: several accounts, AMIs,
/// instance types, states and Regions, one record carrying two product codes and one
/// carrying none, three subscription names each licensed one way, and `DualSubscription`
/// left unset on every record.
///
/// The stopped record is discovered without being an element of the instance report, which
/// "lists the running Amazon EC2 instances that were discovered with commercial Linux
/// subscriptions" (`tests/all/running_instance_report.rs` owns that rule); its subscription
/// is still one of the names the sibling list reports.
const DISCOVERED: &[Row] = &[
    (
        "i-0a1",
        "000000000000",
        "ami-0001",
        "t3.micro",
        "running",
        "us-west-2",
        "RunInstances:0010",
        &["pc-rhel"],
        "RHEL",
        TYPE_INCLUDED,
    ),
    (
        "i-0b2",
        "000000000000",
        "ami-0001",
        "t3.small",
        "running",
        "us-west-2",
        "RunInstances:0010",
        &["pc-rhel", "pc-extra"],
        "RHEL",
        TYPE_INCLUDED,
    ),
    (
        "i-0c3",
        "111111111111",
        "ami-0002",
        "m5.large",
        "stopped",
        "us-east-1",
        "RunInstances",
        &[],
        "SUSE Linux Enterprise Server",
        TYPE_BYOS,
    ),
    (
        "i-0d4",
        "000000000000",
        "ami-0003",
        "t3.micro",
        "running",
        "us-east-1",
        "RunInstances:000g",
        &["pc-ubuntu"],
        "Ubuntu Pro",
        TYPE_MARKETPLACE,
    ),
    (
        "i-0e5",
        "111111111111",
        "ami-0002",
        "m5.large",
        "running",
        "us-east-1",
        "RunInstances",
        &[],
        "SUSE Linux Enterprise Server",
        TYPE_BYOS,
    ),
];

/// One `Filters`-driven list, as a case drives it.
struct Listing {
    /// What a case prints for this list.
    label: &'static str,
    path: &'static str,
    /// The output member carrying the page.
    items: &'static str,
    /// The member of an entry that identifies it — which is also what a cursor of this
    /// list names.
    identity: &'static str,
}

/// `ListLinuxSubscriptionInstances`: a page of the discovered instances, each identified by
/// its `InstanceID`.
const INSTANCES: Listing = Listing {
    label: "ListLinuxSubscriptionInstances",
    path: INSTANCES_PATH,
    items: "Instances",
    identity: "InstanceID",
};

/// `ListLinuxSubscriptions`: a page of what those instances aggregate to, each identified
/// by its subscription `Name`.
const SUBSCRIPTIONS: Listing = Listing {
    label: "ListLinuxSubscriptions",
    path: SUBSCRIPTIONS_PATH,
    items: "Subscriptions",
    identity: "Name",
};

/// An emulator holding [`DISCOVERED`] as the account's discovered inventory: the settings
/// discover exactly the Regions the records were found in, which is what makes them
/// discovered data for both lists.
async fn seeded() -> Harness {
    let h = harness();
    let mut regions: Vec<&str> = DISCOVERED.iter().map(|row| row.5).collect();
    regions.sort_unstable();
    regions.dedup();
    enable_discovery(&h, &regions).await;
    for row in DISCOVERED {
        let (
            instance_id,
            account_id,
            ami_id,
            instance_type,
            status,
            region,
            usage_operation,
            product_codes,
            subscription_name,
            subscription_type,
        ) = *row;
        seed_discovered_instance(
            &h,
            DiscoveredInstanceRecord {
                instance_id: instance_id.to_string(),
                account_id: account_id.to_string(),
                ami_id: ami_id.to_string(),
                instance_type: instance_type.to_string(),
                status: status.to_string(),
                region: region.to_string(),
                usage_operation: usage_operation.to_string(),
                product_codes: product_codes.iter().map(|code| code.to_string()).collect(),
                subscription_name: subscription_name.to_string(),
                subscription_type: subscription_type.to_string(),
                ..DiscoveredInstanceRecord::default()
            },
        );
    }
    h
}

/// One row: the list it drives, the filter's `Name`, the values it compares against, and
/// the identities the list reports for it — the same list whether the request leaves
/// `Operator` out or spells the `Equal` it means.
struct Case {
    listing: &'static Listing,
    name: &'static str,
    values: &'static [&'static str],
    expected: &'static [&'static str],
}

/// The rows, spanning the forms the rule covers on both lists. `Filter$Name` is taken from
/// each list's documented set (`ListLinuxSubscriptionInstancesRequest$Filters`:
/// AccountID, AmiID, DualSubscription, InstanceID, InstanceType, ProductCode, Region,
/// Status, UsageOperation; `ListLinuxSubscriptionsRequest$Filters`: Subscription).
///
/// Every expected list is the EQUALITY of the values named, and differs from what the two
/// other modelled comparisons of the same values would report, so a row pins the default
/// to `Equal` rather than merely to "some comparison": `t3` and `RH` equal nothing while
/// they are contained in values that are held, and each non-empty row leaves out records
/// `NotEqual` would report.
const CASES: &[Case] = &[
    // A scalar member, one value, selecting most of the inventory.
    Case {
        listing: &INSTANCES,
        name: "Status",
        values: &["running"],
        expected: &["i-0a1", "i-0b2", "i-0d4", "i-0e5"],
    },
    // A scalar member, one value, selecting a strict subset.
    Case {
        listing: &INSTANCES,
        name: "AccountID",
        values: &["111111111111"],
        expected: &["i-0e5"],
    },
    // Several values compared against, as the set they are.
    Case {
        listing: &INSTANCES,
        name: "InstanceType",
        values: &["t3.micro", "m5.large"],
        expected: &["i-0a1", "i-0d4", "i-0e5"],
    },
    // The list member: a record satisfies the filter when ANY code it holds equals a
    // value named, and the record holding no code at all satisfies none.
    Case {
        listing: &INSTANCES,
        name: "ProductCode",
        values: &["pc-rhel", "pc-ubuntu"],
        expected: &["i-0a1", "i-0b2", "i-0d4"],
    },
    Case {
        listing: &INSTANCES,
        name: "Region",
        values: &["us-east-1"],
        expected: &["i-0d4", "i-0e5"],
    },
    // A value that is a substring of what the records hold, not what they hold: equality
    // selects nothing, which `Contains` would not.
    Case {
        listing: &INSTANCES,
        name: "InstanceType",
        values: &["t3"],
        expected: &[],
    },
    // A member every record leaves unset: there is nothing on any record to equal.
    Case {
        listing: &INSTANCES,
        name: "DualSubscription",
        values: &["Yes", "No"],
        expected: &[],
    },
    // The sibling list's own `Filter$Name`, over the names the inventory aggregates to.
    Case {
        listing: &SUBSCRIPTIONS,
        name: "Subscription",
        values: &["RHEL", "Ubuntu Pro"],
        expected: &["RHEL", "Ubuntu Pro"],
    },
    Case {
        listing: &SUBSCRIPTIONS,
        name: "Subscription",
        values: &["SUSE Linux Enterprise Server"],
        expected: &["SUSE Linux Enterprise Server"],
    },
    Case {
        listing: &SUBSCRIPTIONS,
        name: "Subscription",
        values: &["RH"],
        expected: &[],
    },
];

/// The two spellings of one case's filter: the `Operator` left absent, and the `Equal` it
/// means spelled out.
fn spellings(case: &Case) -> [Value; 2] {
    let absent = json!({ "Filters": [{ "Name": case.name, "Values": case.values }] });
    let spelled = json!({
        "Filters": [{ "Name": case.name, "Values": case.values, "Operator": "Equal" }]
    });
    [absent, spelled]
}

/// One page of `listing` under `selection`, with `max_results` as the page size and
/// `token` as the continuation, if any.
async fn page(
    h: &Harness,
    listing: &Listing,
    selection: &Value,
    max_results: Option<i32>,
    token: Option<&str>,
) -> Reply {
    let mut body = selection.clone();
    if let Some(max_results) = max_results {
        body["MaxResults"] = json!(max_results);
    }
    if let Some(token) = token {
        body["NextToken"] = json!(token);
    }
    call(h, listing.path, &body).await
}

/// The identities a 200 page of `listing` reports.
fn reported(reply: &Reply, listing: &Listing, case: &str) -> Vec<String> {
    let body = reply.ok(case);
    body.get(listing.items)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no {} in {body:?}", listing.items))
        .iter()
        .map(|entry| {
            entry
                .get(listing.identity)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: no {} in {entry:?}", listing.identity))
                .to_string()
        })
        .collect()
}

/// The `NextToken` a 200 page carries, if it is truncated.
fn next_token(reply: &Reply, case: &str) -> Option<String> {
    reply
        .ok(case)
        .get("NextToken")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[tokio::test]
async fn a_filter_with_no_operator_selects_what_the_equal_it_means_selects() {
    let h = seeded().await;

    for row in CASES {
        let expected: Vec<String> = row.expected.iter().map(|id| (*id).to_string()).collect();
        for (spelling, selection) in ["an absent Operator", "the Equal it means"]
            .into_iter()
            .zip(spellings(row))
        {
            let case = &format!(
                "{} {} {:?} with {spelling}",
                row.listing.label, row.name, row.values
            );
            let actual = reported(
                &page(&h, row.listing, &selection, None, None).await,
                row.listing,
                case,
            );
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_cursor_minted_under_one_spelling_of_the_default_continues_under_the_other() {
    let h = seeded().await;

    for row in CASES {
        if row.expected.len() < 2 {
            // A selection of fewer than two elements is never truncated, so it hands out
            // no cursor to continue; the selection itself is pinned by the test above.
            continue;
        }
        let expected: Vec<String> = row.expected.iter().map(|id| (*id).to_string()).collect();
        let [absent, spelled] = spellings(row);

        // Start the walk under each spelling in turn, ALTERNATING the spelling from page to
        // page: every cursor is minted under one spelling and sent back under the other, in
        // both directions, and the walk still reports the selected list exactly once, in
        // order.
        for first in [0_usize, 1] {
            let case = &format!(
                "{} {} {:?} walked from {}",
                row.listing.label,
                row.name,
                row.values,
                if first == 0 {
                    "an absent Operator"
                } else {
                    "the Equal it means"
                }
            );
            let order = if first == 0 {
                [&absent, &spelled]
            } else {
                [&spelled, &absent]
            };
            let mut spellings = order.into_iter().cycle();
            let mut walked: Vec<String> = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let selection = spellings.next().expect("the spellings cycle is endless");
                let reply = page(&h, row.listing, selection, Some(1), token.as_deref()).await;
                walked.extend(reported(&reply, row.listing, case));
                token = next_token(&reply, case);
                assert!(
                    walked.len() <= expected.len(),
                    "case {case:?}: the walk did not terminate, reported {walked:?} of \
                     {expected:?}"
                );
                if token.is_none() {
                    break;
                }
            }
            assert_eq!(
                walked, expected,
                "case {case:?}: the alternating walk must report the selected list exactly \
                 once, in order, expected {expected:?}, got {walked:?}"
            );
        }
    }
}
