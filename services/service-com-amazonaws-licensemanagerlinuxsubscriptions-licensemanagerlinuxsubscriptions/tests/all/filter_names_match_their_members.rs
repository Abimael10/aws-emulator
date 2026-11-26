// SPDX-License-Identifier: CC0-1.0

//! Each discovery list ACCEPTS exactly the `Filter$Name` values it ADDRESSES a member for,
//! driven through both of them.
//!
//! A `Filters`-driven list has two halves: the request is validated against the names the
//! list documents, and the selection applies the member each admitted name addresses. Those
//! two have to be the same set of names, because either difference is a defect a client
//! sees:
//!
//! * a name validation admits without a member to apply it to selects nothing at all, which
//!   is the silently empty page validation exists to turn into a `ValidationException`;
//! * a member stated for a name validation refuses can never be applied.
//!
//! So this suite asserts the equality itself, over a catalog of candidate names and through
//! both lists: for each candidate, the list either addresses it — and then the filter
//! selects exactly the records whose addressed member holds the named values — or it does
//! not, and then the request is refused with `ValidationException` (HTTP 400). A row whose
//! name is admitted but projects nothing fails the selection assertion, and a row whose
//! name is addressed but refused fails on the status, so neither half can drift away from
//! the other.

use serde_json::{json, Value};

use crate::harness::{call, enable_discovery, harness, seed_discovered_instance, Harness, Reply};
use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::state::discovery::{
    SUBSCRIPTION_TYPE_BYOS, SUBSCRIPTION_TYPE_INCLUDED,
};
use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::state::{
    DiscoveredInstanceRecord, INSTANCE_STATUS_RUNNING,
};

/// The two `Filters`-driven lists (`OPERATIONS.md`).
const SUBSCRIPTIONS: &str = "/subscription/ListLinuxSubscriptions";
const INSTANCES: &str = "/subscription/ListLinuxSubscriptionInstances";

/// One filter row: the `Filter$Name`, the `Values` to select by, and the identities the
/// filter must select, in the order the list reports them.
type Row = (
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

/// One of the two lists: where it is reached, which output member carries its page, which
/// member identifies one of its entries, and one row per `Filter$Name` it addresses.
struct List {
    label: &'static str,
    path: &'static str,
    /// The output member carrying the page's entries.
    key: &'static str,
    /// The entry member that identifies one reported element of this list.
    identity: &'static str,
    addressed: &'static [Row],
}

/// Every `Filter$Name` the instance list documents, with the values that select on the
/// member each of them addresses in [`FIXTURE`] — the names are the model's documentation
/// for `ListLinuxSubscriptionInstancesRequest$Filters` ("you can specify one of the
/// following values for the `Name` key": AccountID, AmiID, DualSubscription, InstanceID,
/// InstanceType, ProductCode, Region, Status, UsageOperation).
///
/// The rows differ along what the rule generalises over: which member is addressed, how
/// many of the records hold the named value (one, both), and whether the addressed member
/// is a scalar or the LIST member `ProductCode` (filtered here both on a code only one
/// record's AMI reports and on one both report).
const INSTANCE_ROWS: &[Row] = &[
    ("AccountID", &["111111111111"], &["i-bbb2"]),
    ("AmiID", &["ami-0001"], &["i-aaa1"]),
    ("DualSubscription", &["Yes"], &["i-bbb2"]),
    ("InstanceID", &["i-aaa1"], &["i-aaa1"]),
    ("InstanceType", &["m5.large"], &["i-bbb2"]),
    ("ProductCode", &["pc-suse"], &["i-bbb2"]),
    ("ProductCode", &["pc-shared"], &["i-aaa1", "i-bbb2"]),
    ("Region", &["us-west-2"], &["i-aaa1"]),
    // Every record this list reports is a running one, so the state they share selects
    // both of them.
    ("Status", &[INSTANCE_STATUS_RUNNING], &["i-aaa1", "i-bbb2"]),
    ("UsageOperation", &["RunInstances:0010"], &["i-aaa1"]),
];

/// The one `Filter$Name` the subscription list documents ("The valid names for this filter
/// are: Subscription", `ListLinuxSubscriptionsRequest$Filters`), which addresses the
/// subscription's own name — selected here for one subscription and for both.
const SUBSCRIPTION_ROWS: &[Row] = &[
    ("Subscription", &[RHEL], &[RHEL]),
    ("Subscription", &[SUSE], &[SUSE]),
    ("Subscription", &[RHEL, SUSE], &[RHEL, SUSE]),
];

const LISTS: &[List] = &[
    List {
        label: "ListLinuxSubscriptions",
        path: SUBSCRIPTIONS,
        key: "Subscriptions",
        identity: "Name",
        addressed: SUBSCRIPTION_ROWS,
    },
    List {
        label: "ListLinuxSubscriptionInstances",
        path: INSTANCES,
        key: "Instances",
        identity: "InstanceID",
        addressed: INSTANCE_ROWS,
    },
];

/// Names a candidate row is drawn from: every name either list documents, plus names
/// neither documents — a documented name in the wrong case, and members of the reported
/// shapes (`Instance$SubscriptionName`, `Instance$OsVersion`, `Subscription$Type`,
/// `Subscription$InstanceCount`) that are NOT in either list's documented `Name` set, so a
/// record holding a value for them is still out of reach of a filter.
const CANDIDATE_NAMES: &[&str] = &[
    "AccountID",
    "AmiID",
    "DualSubscription",
    "InstanceID",
    "InstanceType",
    "ProductCode",
    "Region",
    "Status",
    "UsageOperation",
    "Subscription",
    "subscription",
    "accountid",
    "SubscriptionName",
    "OsVersion",
    "Type",
    "InstanceCount",
];

/// The two subscription names the fixture discovers, each licensed one way, so the
/// subscription list reports one element per name and a name identifies one.
const RHEL: &str = "RHEL";
const SUSE: &str = "SUSE Linux Enterprise Server";

/// One discovered instance for the fixture: instance id, account, AMI, instance type,
/// Region, usage operation, product codes, `DualSubscription`, subscription name and type.
type Fixture = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static str,
    &'static str,
);

/// Two RUNNING discovered instances that differ in EVERY member a filter can address, so a
/// filter naming one record's value must select that record and not the other — and sharing
/// one product code, so the list member can be selected on a value both hold.
const FIXTURE: &[Fixture] = &[
    (
        "i-aaa1",
        "000000000000",
        "ami-0001",
        "t3.nano",
        "us-west-2",
        "RunInstances:0010",
        &["pc-rhel", "pc-shared"],
        "No",
        RHEL,
        SUBSCRIPTION_TYPE_INCLUDED,
    ),
    (
        "i-bbb2",
        "111111111111",
        "ami-0002",
        "m5.large",
        "eu-west-1",
        "RunInstances:000g",
        &["pc-suse", "pc-shared"],
        "Yes",
        SUSE,
        SUBSCRIPTION_TYPE_BYOS,
    ),
];

/// Put [`FIXTURE`] in the store through the shared seeding seam, with the account's
/// settings that make those records discovered data: discovery is enabled for exactly the
/// Regions the records were found in, because what the two lists report is the inventory of
/// the Regions the settings discover now.
async fn discover(h: &Harness) {
    let mut regions: Vec<&str> = FIXTURE.iter().map(|record| record.4).collect();
    regions.sort_unstable();
    regions.dedup();
    enable_discovery(h, &regions).await;
    for fixture in FIXTURE {
        let (
            instance_id,
            account_id,
            ami_id,
            instance_type,
            region,
            usage_operation,
            product_codes,
            dual_subscription,
            subscription_name,
            subscription_type,
        ) = *fixture;
        seed_discovered_instance(
            h,
            DiscoveredInstanceRecord {
                instance_id: instance_id.to_string(),
                account_id: account_id.to_string(),
                ami_id: ami_id.to_string(),
                instance_type: instance_type.to_string(),
                status: INSTANCE_STATUS_RUNNING.to_string(),
                region: region.to_string(),
                usage_operation: usage_operation.to_string(),
                product_codes: product_codes.iter().map(|code| code.to_string()).collect(),
                dual_subscription: Some(dual_subscription.to_string()),
                subscription_name: subscription_name.to_string(),
                subscription_type: subscription_type.to_string(),
                ..DiscoveredInstanceRecord::default()
            },
        );
    }
}

/// The identities a list's page reports, read through the member that identifies one of its
/// entries.
fn reported(reply: &Reply, list: &List, case: &str) -> Vec<String> {
    reply
        .ok(case)
        .get(list.key)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no {} in {:?}", list.key, reply.body))
        .iter()
        .map(|entry| {
            entry
                .get(list.identity)
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no {} in {entry:?}", list.identity)
                })
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn an_addressed_filter_name_selects_on_the_member_it_addresses() {
    for list in LISTS {
        let h = harness();
        discover(&h).await;

        for (name, values, expected) in list.addressed {
            let case = &format!("{} filtered by {name} {values:?}", list.label);
            let reply = call(
                &h,
                list.path,
                &json!({ "Filters": [{ "Name": name, "Values": values }] }),
            )
            .await;
            let selected = reported(&reply, list, case);
            assert_eq!(
                selected, *expected,
                "case {case:?}: expected {expected:?}, got {selected:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_filter_name_a_list_does_not_address_is_refused() {
    for list in LISTS {
        let h = harness();
        discover(&h).await;

        for name in CANDIDATE_NAMES {
            if list.addressed.iter().any(|(addressed, _, _)| addressed == name) {
                continue;
            }
            let case = &format!("{} filtered by the unaddressed name {name:?}", list.label);
            call(
                &h,
                list.path,
                &json!({ "Filters": [{ "Name": name, "Values": ["any value"] }] }),
            )
            .await
            .error(case, 400, "ValidationException");
        }
    }
}
