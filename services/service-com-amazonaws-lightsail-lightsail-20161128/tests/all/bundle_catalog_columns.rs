// SPDX-License-Identifier: CC0-1.0

//! `GetBundles` — WHICH rows the instance-plan catalog publishes: the columns of
//! the plan table a size is published in, and what each column's row carries.
//!
//! The rule under test, asserted over the whole catalog rather than one recorded
//! call: the catalog publishes every plan size once in each column of the plan
//! table — the dual-stack Linux/Unix column, the dual-stack Windows column and
//! the IPv6-only column of each platform — and each reported row carries THAT
//! column's own id token, platform, monthly price and `publicIpv4AddressCount`,
//! while the hardware, SSD size and monthly transfer of the size are shared by
//! the columns that publish the size at them.
//!
//! The addressing is a column of its own because a plan published without a
//! public IPv4 address is its own bundle: `SetIpAddressType$acceptBundleUpdate`
//! documents that "an instance bundle will change when switching from dual-stack
//! or ipv4, to ipv6", and `Bundle$publicIpv4AddressCount` is "an integer that
//! indicates the public ipv4 address count included in the bundle, the value is
//! either 0 or 1" — so an IPv6-only plan is a row with its own id and its own
//! price, not the dual-stack row reported with a different count.
//!
//! Every case drives the service through the same front door a client uses: `POST
//! /` with `X-Amz-Target: Lightsail_20161128.GetBundles` and an `awsJson1_1`
//! body, so the constraint walk, the handler and the paging walk are exercised
//! rather than bypassed.

use std::collections::BTreeSet;

use serde_json::{json, Value};

// The front door every suite in this test target drives the service through.
use super::instance_lifecycle::Emulator;

/// The two values of the modelled `InstancePlatform` enum
/// (`smithy_query(query="shape",shape="InstancePlatform")`).
const LINUX_UNIX: &str = "LINUX_UNIX";
const WINDOWS: &str = "WINDOWS";

/// The generation token every published bundle id carries, as the modelled
/// `Bundle$bundleId` documentation's `micro_x_x` shows.
const GENERATION: &str = "3_0";

/// One published plan size, with the monthly price of that size in each column of
/// the plan table.
///
/// SOURCE: the Lightsail instance-plan table (the "Instances" plans of the
/// Lightsail pricing page, which `GetBundles` answers from) — its Linux/Unix and
/// Windows columns and the IPv6-only column of each. Each figure is the table's
/// own for that plan in that column: the Windows columns price the Windows Server
/// licence the plan carries, and an IPv6-only column prices the same hardware
/// without the public IPv4 address, at a gap from the dual-stack row of the same
/// platform that is $1.50 on the smallest plan, $2 on the next two and $4 from
/// the 4 GB plan up — so no single offset applied to one column produces another.
struct Plan {
    /// `Bundle$instanceType` — the leading token of every id of this size.
    size: &'static str,
    /// The monthly price of the dual-stack Linux/Unix plan of this size.
    linux: f64,
    /// The monthly price of the dual-stack Windows plan of this size.
    windows: f64,
    /// The monthly price of the IPv6-only Linux/Unix plan of this size.
    ipv6_only: f64,
    /// The monthly price of the IPv6-only Windows plan of this size.
    windows_ipv6_only: f64,
}

/// Every size the plan table publishes, smallest first — the catalog's own order.
const PLANS: &[Plan] = &[
    Plan {
        size: "nano",
        linux: 5.0,
        windows: 9.5,
        ipv6_only: 3.5,
        windows_ipv6_only: 8.0,
    },
    Plan {
        size: "micro",
        linux: 7.0,
        windows: 14.0,
        ipv6_only: 5.0,
        windows_ipv6_only: 12.0,
    },
    Plan {
        size: "small",
        linux: 12.0,
        windows: 22.0,
        ipv6_only: 10.0,
        windows_ipv6_only: 20.0,
    },
    Plan {
        size: "medium",
        linux: 24.0,
        windows: 44.0,
        ipv6_only: 20.0,
        windows_ipv6_only: 40.0,
    },
    Plan {
        size: "large",
        linux: 44.0,
        windows: 74.0,
        ipv6_only: 40.0,
        windows_ipv6_only: 70.0,
    },
    Plan {
        size: "xlarge",
        linux: 84.0,
        windows: 124.0,
        ipv6_only: 80.0,
        windows_ipv6_only: 120.0,
    },
    Plan {
        size: "2xlarge",
        linux: 164.0,
        windows: 244.0,
        ipv6_only: 160.0,
        windows_ipv6_only: 240.0,
    },
    Plan {
        size: "4xlarge",
        linux: 384.0,
        windows: 574.0,
        ipv6_only: 380.0,
        windows_ipv6_only: 570.0,
    },
    Plan {
        size: "8xlarge",
        linux: 884.0,
        windows: 1_254.0,
        ipv6_only: 880.0,
        windows_ipv6_only: 1_250.0,
    },
    Plan {
        size: "12xlarge",
        linux: 1_324.0,
        windows: 1_884.0,
        ipv6_only: 1_320.0,
        windows_ipv6_only: 1_880.0,
    },
    Plan {
        size: "16xlarge",
        linux: 1_764.0,
        windows: 2_504.0,
        ipv6_only: 1_760.0,
        windows_ipv6_only: 2_500.0,
    },
];

/// The columns of the plan table, in the order the catalog publishes them: the id
/// token a row of the column carries between its size and its generation, the
/// platform it supports, and the public IPv4 address count it includes.
const COLUMNS: &[(Option<&str>, &str, i64)] = &[
    (None, LINUX_UNIX, 1),
    (Some("win"), WINDOWS, 1),
    (Some("ipv6"), LINUX_UNIX, 0),
    (Some("win_ipv6"), WINDOWS, 0),
];

/// The dual-stack column of each platform paired with the IPv6-only column of the
/// same platform: the two rows of a size that differ only in the public IPv4
/// address and its price.
const ADDRESSING_PAIRS: &[(Option<&str>, Option<&str>)] =
    &[(None, Some("ipv6")), (Some("win"), Some("win_ipv6"))];

/// The id a plan of `size` carries in the column whose token is `token`.
fn id_of(size: &str, token: Option<&str>) -> String {
    match token {
        Some(token) => format!("{size}_{token}_{GENERATION}"),
        None => format!("{size}_{GENERATION}"),
    }
}

/// Every bundle the paged walk reports, in page order.
async fn catalog(emulator: &Emulator, include_inactive: bool) -> Vec<Value> {
    let mut reported: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    // A bound on the walk, so a token that never clears fails the case instead of
    // looping: the published catalog is answered in one or two pages.
    for _ in 0..16 {
        let mut request = json!({"includeInactive": include_inactive});
        if let Some(token) = &token {
            request["pageToken"] = json!(token);
        }
        let body = emulator.ok("GetBundles", request.clone()).await;
        let page = body["bundles"]
            .as_array()
            .unwrap_or_else(|| panic!("case {request:?}: expected a bundles array, got {body:?}"))
            .clone();
        reported.extend(page);
        token = body["nextPageToken"].as_str().map(str::to_string);
        if token.is_none() {
            return reported;
        }
    }
    panic!("case includeInactive={include_inactive}: the paged walk did not finish");
}

/// The reported ids, in the order they were reported.
fn ids(reported: &[Value]) -> Vec<String> {
    reported
        .iter()
        .map(|bundle| bundle["bundleId"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The bundle `id`, or a panic naming the case.
fn row<'a>(reported: &'a [Value], id: &str) -> &'a Value {
    reported
        .iter()
        .find(|bundle| bundle["bundleId"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("case {id:?}: expected the catalog to publish it"))
}

/// A whole-number member of a bundle, or a panic naming the case.
fn int(bundle: &Value, member: &str) -> i64 {
    bundle[member]
        .as_i64()
        .unwrap_or_else(|| panic!("case {bundle:?}: expected an integer {member}"))
}

/// A money or size member of a bundle, or a panic naming the case.
fn float(bundle: &Value, member: &str) -> f64 {
    bundle[member]
        .as_f64()
        .unwrap_or_else(|| panic!("case {bundle:?}: expected a number {member}"))
}

/// Whether `price` is `expected` dollars, to the cent.
fn is_price(price: f64, expected: f64) -> bool {
    (price - expected).abs() < 0.005
}

// ---------------------------------------------------------------------------
// The row set
// ---------------------------------------------------------------------------

/// RULE: every published size is reported exactly once in every published column,
/// each column's rows in the catalog's size order, the columns in the published
/// order — so the whole row set and its order are pinned, not just the columns of
/// a row.
#[tokio::test]
async fn every_published_size_is_reported_once_in_every_published_column() {
    let emulator = Emulator::new();

    // Rows: the flag absent, sent false, and sent true — the catalog holds no
    // retired plan, so each reports the same published set.
    for request_flag in [None, Some(false), Some(true)] {
        let reported = catalog(&emulator, request_flag.unwrap_or(false)).await;
        let got = ids(&reported);

        let want: Vec<String> = COLUMNS
            .iter()
            .flat_map(|(token, _, _)| PLANS.iter().map(move |plan| id_of(plan.size, *token)))
            .collect();
        assert_eq!(
            got, want,
            "case includeInactive={request_flag:?}: expected the published catalog in its own \
             order, got {got:?}"
        );

        let unique: BTreeSet<&String> = got.iter().collect();
        assert_eq!(
            unique.len(),
            got.len(),
            "case includeInactive={request_flag:?}: a bundle was reported twice: {got:?}"
        );
    }
}

/// RULE: a row's platform and public IPv4 address count are its COLUMN's — the
/// Windows column's rows support `WINDOWS` and include the one address, the
/// IPv6-only column's support `LINUX_UNIX` and include none.
#[tokio::test]
async fn each_rows_platform_and_address_count_are_its_columns() {
    let emulator = Emulator::new();
    let reported = catalog(&emulator, true).await;

    for (token, platform, ipv4) in COLUMNS {
        for plan in PLANS {
            let case = id_of(plan.size, *token);
            let bundle = row(&reported, &case);
            assert_eq!(
                bundle["supportedPlatforms"],
                json!([platform]),
                "case {case:?}: expected exactly the platform the column publishes, got {bundle:?}"
            );
            assert_eq!(
                int(bundle, "publicIpv4AddressCount"),
                *ipv4,
                "case {case:?}: expected publicIpv4AddressCount {ipv4}, got {bundle:?}"
            );
            assert_eq!(
                bundle["instanceType"].as_str(),
                Some(plan.size),
                "case {case:?}: expected the id's size token as instanceType, got {bundle:?}"
            );
            assert_eq!(
                bundle["isActive"].as_bool(),
                Some(true),
                "case {case:?}: every published plan is still offered, got {bundle:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The IPv6-only column
// ---------------------------------------------------------------------------

/// RULE: the IPv6-only plan of a size is the Linux/Unix plan of that size WITHOUT
/// the public IPv4 address: it shares the size's hardware, SSD, monthly transfer,
/// power and label with its dual-stack twin, includes no public IPv4 address, and
/// costs less.
#[tokio::test]
async fn an_ipv6_only_plan_drops_the_address_and_keeps_the_sizes_hardware() {
    let emulator = Emulator::new();
    let reported = catalog(&emulator, true).await;

    for (dual_stack_token, ipv6_token) in ADDRESSING_PAIRS {
        for plan in PLANS {
            let case = format!("{} {:?}", plan.size, ipv6_token.unwrap_or_default());
            let dual_stack = row(&reported, &id_of(plan.size, *dual_stack_token));
            let ipv6_only = row(&reported, &id_of(plan.size, *ipv6_token));

            for member in [
                "cpuCount",
                "ramSizeInGb",
                "diskSizeInGb",
                "transferPerMonthInGb",
                "power",
                "instanceType",
                "name",
                "supportedPlatforms",
            ] {
                assert_eq!(
                    dual_stack[member], ipv6_only[member],
                    "case {case}: the two addressing columns of a size and platform share their \
                     {member}, got {dual_stack:?} and {ipv6_only:?}"
                );
            }

            assert_eq!(
                int(dual_stack, "publicIpv4AddressCount"),
                1,
                "case {case}: a dual-stack plan includes the one public IPv4 address, got \
                 {dual_stack:?}"
            );
            assert_eq!(
                int(ipv6_only, "publicIpv4AddressCount"),
                0,
                "case {case}: an IPv6-only plan includes no public IPv4 address, got \
                 {ipv6_only:?}"
            );

            let paid = float(dual_stack, "price");
            let ipv6_price = float(ipv6_only, "price");
            assert!(
                ipv6_price < paid,
                "case {case}: the IPv6-only plan drops the public IPv4 address, so it costs \
                 less than {paid}, got {ipv6_price}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The price columns
// ---------------------------------------------------------------------------

/// RULE: a row's price is the plan table's own figure for that size in that
/// column, never another column's price with an offset applied — which the gaps
/// show: they differ from size to size in both directions.
#[tokio::test]
async fn each_column_reports_its_own_published_price() {
    let emulator = Emulator::new();
    let reported = catalog(&emulator, true).await;

    let mut windows_gaps: BTreeSet<i64> = BTreeSet::new();
    let mut ipv6_gaps: BTreeSet<i64> = BTreeSet::new();

    for plan in PLANS {
        for (token, want) in [
            (None, plan.linux),
            (Some("win"), plan.windows),
            (Some("ipv6"), plan.ipv6_only),
            (Some("win_ipv6"), plan.windows_ipv6_only),
        ] {
            let case = id_of(plan.size, token);
            let got = float(row(&reported, &case), "price");
            assert!(
                is_price(got, want),
                "case {case:?}: expected the plan table's price {want}, got {got}"
            );
        }
        // In cents, so the set holds exact values.
        windows_gaps.insert(((plan.windows - plan.linux) * 100.0).round() as i64);
        ipv6_gaps.insert(((plan.linux - plan.ipv6_only) * 100.0).round() as i64);
    }

    for (column, gaps) in [("the Windows", &windows_gaps), ("the IPv6-only", &ipv6_gaps)] {
        assert!(
            gaps.len() > 1,
            "case {column} column: its prices are published per plan, so the gaps to the \
             Linux/Unix column differ between sizes, got {gaps:?}"
        );
    }
}

/// RULE: the Windows column covers every published size, including the largest
/// ones, each at that column's own price, SSD size and power while sharing the
/// size's vCPU count, RAM and monthly transfer with its Linux/Unix row.
///
/// SOURCE of the figures: the plan table's rows for the two largest sizes — the
/// sizes whose Windows plans this suite's `PLANS` prices at $1,884 and $2,504.
#[tokio::test]
async fn the_windows_column_covers_the_largest_published_sizes() {
    let emulator = Emulator::new();
    let reported = catalog(&emulator, true).await;

    /// One expected Windows row: the size, its vCPU count, RAM, SSD, monthly
    /// transfer and power.
    struct Tier {
        size: &'static str,
        cpu_count: i64,
        ram_size_in_gb: f64,
        disk_size_in_gb: i64,
        transfer_per_month_in_gb: i64,
        power: i64,
    }

    // The two sizes above the ones the earlier catalog served on Windows, plus a
    // mid-table size as the control.
    let tiers = [
        Tier {
            size: "4xlarge",
            cpu_count: 16,
            ram_size_in_gb: 64.0,
            disk_size_in_gb: 1_280,
            transfer_per_month_in_gb: 8_192,
            power: 6_000,
        },
        Tier {
            size: "12xlarge",
            cpu_count: 48,
            ram_size_in_gb: 192.0,
            disk_size_in_gb: 1_280,
            transfer_per_month_in_gb: 10_240,
            power: 7_500,
        },
        Tier {
            size: "16xlarge",
            cpu_count: 64,
            ram_size_in_gb: 256.0,
            disk_size_in_gb: 1_280,
            transfer_per_month_in_gb: 10_240,
            power: 8_000,
        },
    ];

    for tier in &tiers {
        let case = id_of(tier.size, Some("win"));
        let bundle = row(&reported, &case);
        assert_eq!(
            bundle["supportedPlatforms"],
            json!([WINDOWS]),
            "case {case:?}: expected a Windows plan, got {bundle:?}"
        );
        assert_eq!(
            int(bundle, "cpuCount"),
            tier.cpu_count,
            "case {case:?}: expected cpuCount {}, got {bundle:?}",
            tier.cpu_count
        );
        assert!(
            (float(bundle, "ramSizeInGb") - tier.ram_size_in_gb).abs() < 0.01,
            "case {case:?}: expected ramSizeInGb {}, got {bundle:?}",
            tier.ram_size_in_gb
        );
        assert_eq!(
            int(bundle, "diskSizeInGb"),
            tier.disk_size_in_gb,
            "case {case:?}: expected diskSizeInGb {}, got {bundle:?}",
            tier.disk_size_in_gb
        );
        assert_eq!(
            int(bundle, "transferPerMonthInGb"),
            tier.transfer_per_month_in_gb,
            "case {case:?}: expected transferPerMonthInGb {}, got {bundle:?}",
            tier.transfer_per_month_in_gb
        );
        assert_eq!(
            int(bundle, "power"),
            tier.power,
            "case {case:?}: expected power {}, got {bundle:?}",
            tier.power
        );

        // A Windows plan carries an operating-system licence, so it costs more
        // than the Linux/Unix row of the same size.
        let linux_price = float(row(&reported, &id_of(tier.size, None)), "price");
        let windows_price = float(bundle, "price");
        assert!(
            windows_price > linux_price,
            "case {case:?}: a Windows plan carries a licence, so it costs more than \
             {linux_price}, got {windows_price}"
        );
    }
}

// ---------------------------------------------------------------------------
// A published row can be created from
// ---------------------------------------------------------------------------

/// RULE: a plan the catalog reports can be created from, and the instance then
/// reports that plan — so an IPv6-only row is a selectable bundle rather than a
/// catalog line nothing accepts.
#[tokio::test]
async fn an_instance_can_be_created_on_a_reported_ipv6_only_plan() {
    let emulator = Emulator::new();
    let reported = catalog(&emulator, false).await;

    // Rows: the smallest IPv6-only plan and a larger one, each paired with a
    // Linux/Unix blueprint the catalog publishes.
    let rows = [
        ("nano_ipv6_3_0", "ubuntu_22_04", "ipv6-smallest"),
        ("medium_ipv6_3_0", "amazon_linux_2023", "ipv6-medium"),
    ];

    for (bundle_id, blueprint_id, name) in rows {
        let case = bundle_id;
        // The plan is one the catalog reports: the id a client reads out of
        // `GetBundles` is the id it then sends.
        let published = row(&reported, case);
        assert_eq!(
            int(published, "publicIpv4AddressCount"),
            0,
            "case {case}: the catalog reports it as an IPv6-only plan, got {published:?}"
        );

        emulator
            .ok(
                "CreateInstances",
                json!({
                    "instanceNames": [name],
                    "availabilityZone": "us-east-1a",
                    "blueprintId": blueprint_id,
                    "bundleId": bundle_id,
                    "ipAddressType": "ipv6",
                }),
            )
            .await;

        let read = emulator
            .ok("GetInstance", json!({"instanceName": name}))
            .await;
        let instance = &read["instance"];
        assert_eq!(
            instance["bundleId"].as_str(),
            Some(bundle_id),
            "case {case}: expected the instance to report the plan it was created on, got \
             {instance:?}"
        );
        assert_eq!(
            instance["ipAddressType"].as_str(),
            Some("ipv6"),
            "case {case}: expected an IPv6-only instance, got {instance:?}"
        );
        assert!(
            instance["publicIpAddress"].is_null(),
            "case {case}: an IPv6-only instance holds no public IPv4 address, got {instance:?}"
        );
        assert!(
            instance["ipv6Addresses"]
                .as_array()
                .is_some_and(|addresses| !addresses.is_empty()),
            "case {case}: expected at least one IPv6 address, got {instance:?}"
        );
        // The plan's own hardware, from the same table the catalog answers from.
        assert_eq!(
            instance["hardware"]["cpuCount"].as_i64(),
            Some(int(published, "cpuCount")),
            "case {case}: expected the plan's vCPU count, got {instance:?}"
        );
    }
}
