// SPDX-License-Identifier: CC0-1.0

//! The CONTENTS of this service's published reference tables, driven over the
//! wire through the same front door a client uses.
//!
//! The rule under test, over every column a published catalog carries per entry:
//! a reported column is the figure or the text the catalog publishes for THAT
//! entry — and, for an entry published per platform, for that platform's column
//! of the table — never a value the emulator derives from another column of the
//! same row.
//!
//! What that comes to, operation by operation:
//!
//! * `GetBlueprints` reports each image's own catalog text in `description`, and
//!   the links AWS serves for the image: the AWS page the image is described at
//!   in `productUrl` — its Marketplace listing, or the product page AWS keeps for
//!   an operating system of its own — and the end-user licence agreement AWS
//!   publishes for it in `licenseUrl` — not a sentence built from the entry's
//!   name and `type`, and not the upstream vendor's own product or licensing
//!   page.
//! * `GetBlueprints` reports in `version` the release of the software an entry's
//!   image installs and in `group` the family token that release is grouped
//!   under, both carried per entry, so no two releases of a family are reported
//!   alike and the listing is not one version repeated.
//! * the blueprint catalog publishes the Windows SQL Server application images
//!   as a tier of their own, after the Windows operating-system images and before
//!   the first Linux/Unix image, each carrying the plan power the database engine
//!   needs.
//! * `GetBundles` reports each plan's `price`, `diskSizeInGb`,
//!   `transferPerMonthInGb` and `power` from the plan table's column for the
//!   plan's platform, so the columns stop at the figures the table stops at
//!   instead of growing with the plan's hardware, the Windows plan of a size
//!   carries that column's own figures, and the price a client reads to cost a
//!   plan is the price the operation that bills it charges.
//! * an instance created on a plan reports the SSD of that plan's own platform
//!   column, so the catalog's figure and the instance's hardware are one.
//! * `GetRegions` describes each Region with the sentence AWS publishes for it.
//!
//! Every case is a TABLE spanning what the rule generalises over — both image
//! classes and both platforms, licensed images and openly licensed ones, the
//! plans whose columns are still growing and the plans at the figures the columns
//! stop at — and each table is backed by catalog-wide invariants asserted over
//! whatever the catalog holds rather than over the rows written out here.
//!
//! The HTTP harness is the one `instance_lifecycle.rs` already has.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::instance_lifecycle::Emulator;

/// The two values of the modelled `InstancePlatform` enum
/// (`smithy_query(query="shape",shape="InstancePlatform")`).
const LINUX_UNIX: &str = "LINUX_UNIX";
const WINDOWS: &str = "WINDOWS";

/// Where a published blueprint's `productUrl` leads: a page on the AWS site —
/// the `aws.amazon.com/marketplace/pp/prodview-<listing>` listing an image is
/// offered through, or the product page AWS keeps for an operating system of its
/// own.
///
/// SOURCE: the `productUrl` values the recorded `GetBlueprints` answer reports,
/// which are all under this host. Written out here rather than read off the
/// crate, so this suite pins the host.
const AWS_PRODUCT_PAGE_PREFIX: &str = "https://aws.amazon.com/";

/// The path under [`AWS_PRODUCT_PAGE_PREFIX`] an image offered through an AWS
/// Marketplace listing is published at.
const MARKETPLACE_LISTING_PATH: &str = "marketplace/pp/prodview-";

/// Where a published blueprint's `licenseUrl` leads: the end-user licence
/// agreement AWS serves for the image, at
/// `<host>/eula/product/<product>/<agreement>.txt`.
///
/// SOURCE: the `licenseUrl` values the recorded answer reports.
const LICENSE_AGREEMENT_PREFIX: &str = "https://d7umqicpi7263.cloudfront.net/eula/product/";

/// Every entry of a reported list, over however many pages it takes.
async fn listing(emulator: &Emulator, operation: &str, member: &str, request: Value) -> Vec<Value> {
    let mut entries = Vec::new();
    let mut token: Option<String> = None;
    for page in 0..50usize {
        let mut body = request.clone();
        if let Some(token) = &token {
            body["pageToken"] = json!(token);
        }
        let answer = emulator.ok(operation, body).await;
        entries.extend(
            answer[member]
                .as_array()
                .unwrap_or_else(|| {
                    panic!("case {operation}: expected a {member} array, got {answer:?}")
                })
                .clone(),
        );
        match answer["nextPageToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => return entries,
        }
        assert!(
            page < 49,
            "case {operation}: the paged walk did not terminate, got {} entries",
            entries.len()
        );
    }
    panic!("case {operation}: the paged walk did not terminate")
}

/// A string member of a reported entry, or a panic naming the case.
fn text(entry: &Value, member: &str) -> String {
    entry[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {entry:?}: expected a string {member}"))
        .to_string()
}

// ---------------------------------------------------------------------------
// GetBlueprints — the catalog's own text, and the links this service serves
// ---------------------------------------------------------------------------

/// One published image, as this rule reaches it.
///
/// `names` is wording the catalog's text for THAT image has to carry — the
/// software the entry is an image of — which is what makes the column the
/// entry's own text rather than one paragraph reused across the catalog.
/// `publishes_license` is whether the catalog publishes the end-user licence
/// agreement AWS serves for the image, which every image it offers carries — a
/// recorded Amazon Linux entry, whose software is offered under its own open
/// terms, carries one too.
///
/// The rows span what the rule generalises over: an operating-system image and
/// an application image, a Windows image and Linux/Unix ones, an image the
/// catalog names after the product it installs, a retired (inactive) entry, an
/// image offered through a Marketplace listing and one AWS publishes a product
/// page of its own for.
struct Image {
    blueprint_id: &'static str,
    names: &'static str,
    publishes_license: bool,
}

const IMAGES: &[Image] = &[
    Image {
        blueprint_id: "windows_server_2025",
        names: "Windows Server 2025",
        publishes_license: true,
    },
    Image {
        blueprint_id: "windows_server_2016",
        names: "Windows Server 2016",
        publishes_license: true,
    },
    Image {
        blueprint_id: "windows_server_2022_sql_2019_express",
        names: "SQL Server 2019 Express",
        publishes_license: true,
    },
    Image {
        blueprint_id: "amazon_linux_2023",
        names: "Amazon Linux 2023",
        publishes_license: true,
    },
    Image {
        blueprint_id: "freebsd_14",
        names: "FreeBSD 14",
        publishes_license: true,
    },
    // A retired entry is still an entry: it carries the same columns, which is
    // what a caller reading an existing instance's image needs.
    Image {
        blueprint_id: "debian_10",
        names: "Debian 10",
        publishes_license: true,
    },
    Image {
        blueprint_id: "wordpress",
        names: "WordPress",
        publishes_license: true,
    },
    Image {
        blueprint_id: "plesk_ubuntu_18_0_35",
        names: "Plesk",
        publishes_license: true,
    },
];

/// The whole blueprint catalog, retired entries included.
async fn blueprint_catalog(emulator: &Emulator) -> Vec<Value> {
    listing(
        emulator,
        "GetBlueprints",
        "blueprints",
        json!({"includeInactive": true}),
    )
    .await
}

/// The entry `blueprint_id` names, or a panic naming the case.
fn image_of(blueprints: &[Value], blueprint_id: &str) -> Value {
    blueprints
        .iter()
        .find(|blueprint| text(blueprint, "blueprintId") == blueprint_id)
        .cloned()
        .unwrap_or_else(|| {
            let ids: Vec<String> = blueprints
                .iter()
                .map(|blueprint| text(blueprint, "blueprintId"))
                .collect();
            panic!("case {blueprint_id}: expected the catalog to publish it, got {ids:?}")
        })
}

#[tokio::test]
async fn every_published_image_reports_its_own_catalog_text() {
    let emulator = Emulator::new();
    let blueprints = blueprint_catalog(&emulator).await;

    for want in IMAGES {
        let case = want.blueprint_id;
        let reported = image_of(&blueprints, case);
        let description = text(&reported, "description");
        let name = text(&reported, "name");
        let blueprint_type = text(&reported, "type");

        assert!(
            description.contains(want.names),
            "case {case}: expected the catalog's text for this image to name {:?}, got \
             {description:?}",
            want.names
        );
        // And it is the catalog's TEXT, not a sentence assembled from the two
        // columns a caller can already read: those two forms are what a derived
        // description would produce for this entry.
        for derived in [
            format!("{name} operating system image for a Lightsail instance."),
            format!("{name} application stack, preinstalled on a Lightsail instance."),
        ] {
            assert_ne!(
                description, derived,
                "case {case}: the description must be the catalog's own text for the image, not \
                 one derived from its name and type ({blueprint_type})"
            );
        }
    }

    // Catalog-wide: every entry carries text of its own, so no two entries share
    // a description and none is a bare restatement of the entry's name.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for blueprint in &blueprints {
        let case = text(blueprint, "blueprintId");
        let description = text(blueprint, "description");
        assert!(
            description.len() > text(blueprint, "name").len() + 40,
            "case {case}: expected the catalog's published text for the image, got \
             {description:?}"
        );
        assert!(
            seen.insert(description.clone()),
            "case {case}: a description is the entry's OWN text, so no two entries share one, \
             got {description:?}"
        );
    }
}

#[tokio::test]
async fn every_published_image_links_to_the_pages_this_service_serves() {
    let emulator = Emulator::new();
    let blueprints = blueprint_catalog(&emulator).await;

    for want in IMAGES {
        let case = want.blueprint_id;
        let reported = image_of(&blueprints, case);

        let product_url = text(&reported, "productUrl");
        assert!(
            product_url.starts_with(AWS_PRODUCT_PAGE_PREFIX)
                && product_url.len() > AWS_PRODUCT_PAGE_PREFIX.len(),
            "case {case}: productUrl must be the AWS page this image is described at, got \
             {product_url:?}"
        );

        let license_url = reported["licenseUrl"].as_str().map(str::to_string);
        assert_eq!(
            license_url.is_some(),
            want.publishes_license,
            "case {case}: expected licenseUrl to be published={}, got {license_url:?}",
            want.publishes_license
        );
        if let Some(license_url) = license_url {
            let agreement = license_url
                .strip_prefix(LICENSE_AGREEMENT_PREFIX)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case}: licenseUrl must be the agreement AWS serves for the image, \
                         got {license_url:?}"
                    )
                });
            // The agreement path names the product and the revision of the
            // agreement published for it, as a text document.
            let parts: Vec<&str> = agreement.split('/').collect();
            assert!(
                parts.len() == 2
                    && !parts[0].is_empty()
                    && parts[1].ends_with(".txt")
                    && parts[1].len() > ".txt".len(),
                "case {case}: expected a <product>/<agreement>.txt agreement path, got \
                 {agreement:?}"
            );
        }
    }

    // Catalog-wide: every link a published entry carries is served by AWS — the
    // page describing the image, the agreement on AWS's own licence host — no two
    // entries point at one page, and every entry publishes the agreement.
    let mut listings: BTreeSet<String> = BTreeSet::new();
    for blueprint in &blueprints {
        let case = text(blueprint, "blueprintId");
        let product_url = text(blueprint, "productUrl");
        assert!(
            product_url.starts_with(AWS_PRODUCT_PAGE_PREFIX),
            "case {case}: productUrl must be under {AWS_PRODUCT_PAGE_PREFIX}, got \
             {product_url:?}"
        );
        assert!(
            listings.insert(product_url.clone()),
            "case {case}: a page is the entry's own, so no two entries share one, got \
             {product_url:?}"
        );
        let license_url = text(blueprint, "licenseUrl");
        assert!(
            license_url.starts_with(LICENSE_AGREEMENT_PREFIX)
                && license_url.len() > LICENSE_AGREEMENT_PREFIX.len(),
            "case {case}: licenseUrl must be under {LICENSE_AGREEMENT_PREFIX}, got \
             {license_url:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// GetBlueprints — the Windows SQL Server application tier
// ---------------------------------------------------------------------------

/// One published Windows SQL Server application image: an edition of the
/// database engine preinstalled on a Windows Server image.
///
/// `min_power` is the power of the smallest plan the image runs on, which is
/// larger than a bare Windows image's because the engine runs beside the
/// operating system. The rows are the whole tier.
struct SqlImage {
    blueprint_id: &'static str,
    name: &'static str,
    group: &'static str,
    min_power: i64,
}

/// The power of the 4 GB plan, which an image carrying the database engine
/// needs.
const SQL_IMAGE_MIN_POWER: i64 = 2_000;

const SQL_IMAGES: &[SqlImage] = &[
    SqlImage {
        blueprint_id: "windows_server_2022_sql_2022_express",
        name: "SQL Server 2022 Express",
        group: "windows_2022_sql_exp_2022",
        min_power: SQL_IMAGE_MIN_POWER,
    },
    SqlImage {
        blueprint_id: "windows_server_2022_sql_2019_express",
        name: "SQL Server 2019 Express",
        group: "windows_2022_sql_exp_2019",
        min_power: SQL_IMAGE_MIN_POWER,
    },
];

#[tokio::test]
async fn the_windows_sql_tier_is_published_between_the_windows_and_linux_images() {
    let emulator = Emulator::new();
    let blueprints = blueprint_catalog(&emulator).await;
    let ids: Vec<String> = blueprints
        .iter()
        .map(|blueprint| text(blueprint, "blueprintId"))
        .collect();
    let position = |blueprint_id: &str| -> usize {
        ids.iter()
            .position(|id| id == blueprint_id)
            .unwrap_or_else(|| panic!("case {blueprint_id}: expected the catalog to publish it"))
    };

    let first_linux = blueprints
        .iter()
        .position(|blueprint| text(blueprint, "platform") == LINUX_UNIX)
        .unwrap_or_else(|| panic!("case the whole catalog: expected a Linux/Unix image"));

    for want in SQL_IMAGES {
        let case = want.blueprint_id;
        let reported = image_of(&blueprints, case);

        assert_eq!(
            text(&reported, "name"),
            want.name,
            "case {case}: expected the catalog's own name for the image, got {reported:?}"
        );
        assert_eq!(
            text(&reported, "group"),
            want.group,
            "case {case}: expected the published family name, got {reported:?}"
        );
        assert_eq!(
            text(&reported, "type"),
            "app",
            "case {case}: an image that preinstalls the engine on an operating system is an \
             application image, got {reported:?}"
        );
        assert_eq!(
            text(&reported, "platform"),
            WINDOWS,
            "case {case}: expected the Windows platform, got {reported:?}"
        );
        assert_eq!(
            reported["minPower"].as_i64(),
            Some(want.min_power),
            "case {case}: expected the plan power the engine needs, {}, got {reported:?}",
            want.min_power
        );

        // The tier sits after the Windows operating-system images and before the
        // first Linux/Unix one, so a caller walking the catalog by position
        // reaches it where the catalog publishes it.
        let at = position(case);
        assert!(
            at < first_linux,
            "case {case}: the Windows SQL tier precedes every Linux/Unix image, got position \
             {at} with the first Linux/Unix image at {first_linux}"
        );
        let windows_os = ids
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                *index < first_linux && text(&blueprints[*index], "type") == "os"
            })
            .map(|(index, _)| index)
            .max()
            .unwrap_or_else(|| panic!("case {case}: expected a Windows operating-system image"));
        assert!(
            at > windows_os,
            "case {case}: the Windows SQL tier follows the Windows operating-system images, got \
             position {at} with the last of them at {windows_os}"
        );
    }
}

// ---------------------------------------------------------------------------
// GetBundles — each platform's column of the plan table
// ---------------------------------------------------------------------------

/// One plan as its own platform's column of the plan table publishes it.
///
/// SOURCE: the "Instances" plans of the Lightsail pricing page, which
/// `GetBundles` answers from. The rows span what the rule generalises over: a
/// plan whose SSD and transfer columns are still growing, the plan each column
/// stops at, the plans above it that repeat the figure it stopped at, the Windows
/// column whose SSD size and power differ from its Linux/Unix row's, and a
/// Windows column that agrees with it.
struct PlanColumn {
    bundle_id: &'static str,
    platform: &'static str,
    disk_size_in_gb: i64,
    transfer_per_month_in_gb: i64,
    power: i64,
}

/// The figure the plan table's SSD column stops at, which every plan from the
/// 64 GB one up carries.
const PUBLISHED_SSD_CEILING: i64 = 1_280;

/// The figure the plan table's monthly transfer column stops at.
const PUBLISHED_TRANSFER_CEILING: i64 = 10_240;

const PLAN_COLUMNS: &[PlanColumn] = &[
    PlanColumn {
        bundle_id: "nano_3_0",
        platform: LINUX_UNIX,
        disk_size_in_gb: 20,
        transfer_per_month_in_gb: 1_024,
        power: 298,
    },
    PlanColumn {
        bundle_id: "nano_win_3_0",
        platform: WINDOWS,
        disk_size_in_gb: 30,
        transfer_per_month_in_gb: 1_024,
        power: 300,
    },
    PlanColumn {
        bundle_id: "large_3_0",
        platform: LINUX_UNIX,
        disk_size_in_gb: 160,
        transfer_per_month_in_gb: 5_120,
        power: 3_000,
    },
    PlanColumn {
        bundle_id: "large_win_3_0",
        platform: WINDOWS,
        disk_size_in_gb: 160,
        transfer_per_month_in_gb: 5_120,
        power: 3_000,
    },
    PlanColumn {
        bundle_id: "4xlarge_3_0",
        platform: LINUX_UNIX,
        disk_size_in_gb: PUBLISHED_SSD_CEILING,
        transfer_per_month_in_gb: 8_192,
        power: 6_000,
    },
    PlanColumn {
        bundle_id: "8xlarge_3_0",
        platform: LINUX_UNIX,
        disk_size_in_gb: PUBLISHED_SSD_CEILING,
        transfer_per_month_in_gb: 9_216,
        power: 7_000,
    },
    PlanColumn {
        bundle_id: "12xlarge_3_0",
        platform: LINUX_UNIX,
        disk_size_in_gb: PUBLISHED_SSD_CEILING,
        transfer_per_month_in_gb: PUBLISHED_TRANSFER_CEILING,
        power: 7_500,
    },
    PlanColumn {
        bundle_id: "16xlarge_3_0",
        platform: LINUX_UNIX,
        disk_size_in_gb: PUBLISHED_SSD_CEILING,
        transfer_per_month_in_gb: PUBLISHED_TRANSFER_CEILING,
        power: 8_000,
    },
];

#[tokio::test]
async fn every_published_plan_reports_its_own_platforms_column_of_the_plan_table() {
    let emulator = Emulator::new();
    let bundles = listing(&emulator, "GetBundles", "bundles", json!({})).await;
    let ids: Vec<String> = bundles
        .iter()
        .map(|bundle| text(bundle, "bundleId"))
        .collect();

    for want in PLAN_COLUMNS {
        let case = want.bundle_id;
        let reported = bundles
            .iter()
            .find(|bundle| text(bundle, "bundleId") == case)
            .unwrap_or_else(|| {
                panic!("case {case}: expected the catalog to publish this plan, got {ids:?}")
            });

        assert_eq!(
            reported["supportedPlatforms"],
            json!([want.platform]),
            "case {case}: expected the plan to support {:?}, got {reported:?}",
            want.platform
        );
        for (member, figure, got) in [
            (
                "diskSizeInGb",
                want.disk_size_in_gb,
                reported["diskSizeInGb"].as_i64(),
            ),
            (
                "transferPerMonthInGb",
                want.transfer_per_month_in_gb,
                reported["transferPerMonthInGb"].as_i64(),
            ),
            ("power", want.power, reported["power"].as_i64()),
        ] {
            assert_eq!(
                got,
                Some(figure),
                "case {case}: expected the plan table's {member} for this plan, {figure}, got \
                 {got:?}"
            );
        }
    }

    // Catalog-wide: no reported plan carries more storage or transfer than the
    // columns publish, which a figure scaled with the plan's vCPU count or RAM
    // would, and the Windows tier is served.
    for bundle in &bundles {
        let case = text(bundle, "bundleId");
        let disk = bundle["diskSizeInGb"].as_i64().unwrap_or_default();
        let transfer = bundle["transferPerMonthInGb"].as_i64().unwrap_or_default();
        assert!(
            disk > 0 && disk <= PUBLISHED_SSD_CEILING,
            "case {case}: the SSD column stops at {PUBLISHED_SSD_CEILING} GB, got {disk}"
        );
        assert!(
            transfer > 0 && transfer <= PUBLISHED_TRANSFER_CEILING,
            "case {case}: the transfer column stops at {PUBLISHED_TRANSFER_CEILING} GB, got \
             {transfer}"
        );
    }
    let windows: Vec<&String> = ids
        .iter()
        .filter(|id| id.split('_').any(|token| token == "win"))
        .collect();
    assert!(
        !windows.is_empty(),
        "case the whole catalog: a Windows blueprint needs a Windows plan, so the catalog must \
         publish the Windows tier, got {ids:?}"
    );
    for bundle in &bundles {
        let case = text(bundle, "bundleId");
        let platform = if case.split('_').any(|token| token == "win") {
            WINDOWS
        } else {
            LINUX_UNIX
        };
        assert_eq!(
            bundle["supportedPlatforms"],
            json!([platform]),
            "case {case}: a plan supports the platform its id names, got {bundle:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The plan's SSD, as the instance created on it reports it
// ---------------------------------------------------------------------------

/// One instance created on a plan, and the SSD its plan's own platform column
/// publishes.
///
/// Each row is a pairing the service would accept — the plan's `power` is at
/// least the blueprint's `minPower`, and a Windows image runs on a Windows plan
/// — and the rows span both platforms, the two Windows sizes a Windows image
/// reaches, and an application image with a power floor of its own.
struct RootDisk {
    case: &'static str,
    blueprint_id: &'static str,
    bundle_id: &'static str,
    size_in_gb: i64,
}

const ROOT_DISKS: &[RootDisk] = &[
    RootDisk {
        case: "the smallest Linux/Unix plan",
        blueprint_id: "amazon_linux_2023",
        bundle_id: "nano_3_0",
        size_in_gb: 20,
    },
    RootDisk {
        case: "the smallest Windows plan a Windows image runs on",
        blueprint_id: "windows_server_2025",
        bundle_id: "micro_win_3_0",
        size_in_gb: 40,
    },
    RootDisk {
        case: "a Windows plan carrying an image with a power floor of its own",
        blueprint_id: "windows_server_2022_sql_2019_express",
        bundle_id: "medium_win_3_0",
        size_in_gb: 80,
    },
];

#[tokio::test]
async fn an_instance_reports_the_ssd_its_plans_platform_column_publishes() {
    let emulator = Emulator::new();

    for row in ROOT_DISKS {
        let case = row.case;
        let name = format!("disk-{}", row.bundle_id);
        emulator
            .ok(
                "CreateInstances",
                json!({
                    "instanceNames": [name],
                    "availabilityZone": "us-east-1a",
                    "blueprintId": row.blueprint_id,
                    "bundleId": row.bundle_id,
                }),
            )
            .await;

        // Read the instance back: the plan the catalog reports and the hardware
        // the instance runs on are one figure, not two.
        let instance = emulator
            .ok("GetInstance", json!({"instanceName": name}))
            .await["instance"]
            .clone();
        assert_eq!(
            instance["bundleId"].as_str(),
            Some(row.bundle_id),
            "case {case}: expected the instance to run on the plan it was created on, got \
             {instance:?}"
        );
        let system_disk = instance["hardware"]["disks"][0].clone();
        assert_eq!(
            system_disk["sizeInGb"].as_i64(),
            Some(row.size_in_gb),
            "case {case}: expected the SSD the plan's own platform column publishes, {}, got \
             {system_disk:?}",
            row.size_in_gb
        );

        // And the listing agrees with the instance, which is the whole point of
        // resolving both from one column.
        let bundles = listing(&emulator, "GetBundles", "bundles", json!({})).await;
        let listed = bundles
            .iter()
            .find(|bundle| text(bundle, "bundleId") == row.bundle_id)
            .unwrap_or_else(|| panic!("case {case}: expected the plan to be published"));
        assert_eq!(
            listed["diskSizeInGb"].as_i64(),
            Some(row.size_in_gb),
            "case {case}: the catalog and the instance report one SSD size, got {listed:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// GetRegions — the sentence published for each Region
// ---------------------------------------------------------------------------

/// The sentence stem `Region$description` carries for a Region that is enabled
/// by default, as that member's own documentation shows ("This region is
/// recommended to serve users in the eastern United States and eastern Canada").
const DESCRIPTION_STEM: &str = "This region is recommended to serve users in ";

/// The geography clause AWS publishes for a Region, after the stem.
///
/// The rows span a clause naming a wider area around the Region's own territory,
/// one naming a country alone, and one naming several territories — and include
/// the Region whose published sentence this fix corrected.
const REGION_CLAUSES: &[(&str, &str)] = &[
    ("ca-central-1", "eastern and central Canada"),
    ("ap-northeast-1", "Japan"),
    (
        "us-west-2",
        "the northwestern United States, Alaska, and western Canada",
    ),
];

#[tokio::test]
async fn every_region_is_described_by_the_sentence_published_for_it() {
    let emulator = Emulator::new();
    let regions = emulator.ok("GetRegions", json!({})).await["regions"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    for (name, clause) in REGION_CLAUSES {
        let reported = regions
            .iter()
            .find(|region| text(region, "name") == *name)
            .unwrap_or_else(|| panic!("case {name}: expected this Region to be served"));
        let expected = format!("{DESCRIPTION_STEM}{clause}");
        assert_eq!(
            text(reported, "description"),
            expected,
            "case {name}: expected the published sentence {expected:?}, got {reported:?}"
        );
        // The clause names the geography the Region is recommended for, which is
        // more than the place the Region is named after: a sentence that only
        // restated `displayName` would tell a caller nothing.
        assert_ne!(
            *clause,
            text(reported, "displayName"),
            "case {name}: the clause must name the served geography, not the display name"
        );
    }
}

// ---------------------------------------------------------------------------
// GetBundles — the price in each platform's column of the plan table
// ---------------------------------------------------------------------------

/// The hours AWS prices one Lightsail monthly plan at: a monthly plan is billed
/// by the hour, so a window of 730 hours is exactly one plan's worth.
const BILLING_MONTH_HOURS: f64 = 730.0;

/// How far after now an estimated window opens, so the resource it is about
/// already exists for the whole of it.
const WINDOW_STARTS_IN_SECONDS: f64 = 86_400.0;

/// Now, in epoch seconds, as a `GetCostEstimate` window is expressed.
fn now_epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64()
}

/// Whether a reported dollar figure is `expected`, to the cent.
fn is_price(reported: Option<f64>, expected: f64) -> bool {
    reported.is_some_and(|price| (price - expected).abs() < 0.005)
}

/// One plan and the monthly price its own platform's column of the plan table
/// publishes for it.
///
/// SOURCE: the "Instances" plans of the Lightsail pricing page, which
/// `GetBundles` answers from: the Linux/Unix column at $5, $7, $12, $24, $44,
/// $84, $164 and $384 a month for the plans from 512 MB to 64 GB of RAM, and the
/// WINDOWS column at $9.50, $14, $22, $44 and $74 for the plans up to 8 GB, each
/// its own published figure.
///
/// The rows span what the rule generalises over: both platforms, the Windows
/// tiers whose prices a recorded answer quotes and the Windows plans below and
/// above them, the largest plan published on Linux/Unix alone, and the pair of
/// plans the two columns price identically (the 4 GB Windows plan and the 8 GB
/// Linux/Unix one), which no per-size markup of one column produces.
struct PlanPrice {
    bundle_id: &'static str,
    platform: &'static str,
    price: f64,
}

const PLAN_PRICES: &[PlanPrice] = &[
    PlanPrice {
        bundle_id: "nano_3_0",
        platform: LINUX_UNIX,
        price: 5.0,
    },
    PlanPrice {
        bundle_id: "micro_3_0",
        platform: LINUX_UNIX,
        price: 7.0,
    },
    PlanPrice {
        bundle_id: "large_3_0",
        platform: LINUX_UNIX,
        price: 44.0,
    },
    PlanPrice {
        bundle_id: "16xlarge_3_0",
        platform: LINUX_UNIX,
        price: 1_764.0,
    },
    PlanPrice {
        bundle_id: "nano_win_3_0",
        platform: WINDOWS,
        price: 9.5,
    },
    PlanPrice {
        bundle_id: "micro_win_3_0",
        platform: WINDOWS,
        price: 14.0,
    },
    PlanPrice {
        bundle_id: "small_win_3_0",
        platform: WINDOWS,
        price: 22.0,
    },
    PlanPrice {
        bundle_id: "medium_win_3_0",
        platform: WINDOWS,
        price: 44.0,
    },
    PlanPrice {
        bundle_id: "large_win_3_0",
        platform: WINDOWS,
        price: 74.0,
    },
    PlanPrice {
        bundle_id: "xlarge_win_3_0",
        platform: WINDOWS,
        price: 140.0,
    },
];

#[tokio::test]
async fn every_published_plan_is_priced_by_its_own_platforms_column_of_the_plan_table() {
    let emulator = Emulator::new();
    let bundles = listing(&emulator, "GetBundles", "bundles", json!({})).await;
    let ids: Vec<String> = bundles
        .iter()
        .map(|bundle| text(bundle, "bundleId"))
        .collect();

    for want in PLAN_PRICES {
        let case = want.bundle_id;
        let reported = bundles
            .iter()
            .find(|bundle| text(bundle, "bundleId") == case)
            .unwrap_or_else(|| {
                panic!("case {case}: expected the catalog to publish this plan, got {ids:?}")
            });
        assert_eq!(
            reported["supportedPlatforms"],
            json!([want.platform]),
            "case {case}: expected the plan to support {:?}, got {reported:?}",
            want.platform
        );
        assert!(
            is_price(reported["price"].as_f64(), want.price),
            "case {case}: expected the plan table's monthly price for this plan, {}, got {:?}",
            want.price,
            reported["price"]
        );
    }

    // Catalog-wide, over whatever the catalog holds: every plan carries a price,
    // and within a platform a plan of more power costs more than one of less.
    let mut priced: Vec<(String, String, i64, f64)> = Vec::new();
    for bundle in &bundles {
        let case = text(bundle, "bundleId");
        let price = bundle["price"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {case}: expected a numeric price, got {bundle:?}"));
        let power = bundle["power"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case}: expected a numeric power, got {bundle:?}"));
        let platform = bundle["supportedPlatforms"][0]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a platform, got {bundle:?}"))
            .to_string();
        assert!(
            price > 0.0,
            "case {case}: a published plan has a monthly price, got {price}"
        );
        priced.push((case, platform, power, price));
    }
    for platform in [LINUX_UNIX, WINDOWS] {
        let mut of_platform: Vec<&(String, String, i64, f64)> = priced
            .iter()
            .filter(|(_, reported, _, _)| reported == platform)
            .collect();
        of_platform.sort_by_key(|(_, _, power, _)| *power);
        for pair in of_platform.windows(2) {
            let (cheaper, dearer) = (pair[0], pair[1]);
            if cheaper.2 == dearer.2 {
                continue;
            }
            assert!(
                dearer.3 > cheaper.3,
                "case {} vs {}: a plan of more power costs more on {platform}, got {} and {}",
                cheaper.0,
                dearer.0,
                cheaper.3,
                dearer.3
            );
        }
    }

    // And the two columns are priced independently: the Windows plan of a size
    // costs more than its Linux/Unix row, but by no fixed factor — a Windows
    // price computed as the Linux/Unix one times a markup would make every ratio
    // one and the same figure.
    let by_id: BTreeMap<&String, f64> = priced
        .iter()
        .map(|(id, _, _, price)| (id, *price))
        .collect();
    let mut ratios: BTreeSet<String> = BTreeSet::new();
    let mut pairs = 0usize;
    for (id, _, _, windows_price) in &priced {
        let Some(size) = id.strip_suffix("_win_3_0") else {
            continue;
        };
        let linux_id = format!("{size}_3_0");
        let Some(linux_price) = by_id.get(&linux_id) else {
            panic!("case {id}: expected the Linux/Unix plan {linux_id} to be published too")
        };
        assert!(
            windows_price > linux_price,
            "case {id}: the Windows column of a size costs more than its Linux/Unix row, \
             {linux_price}, got {windows_price}"
        );
        ratios.insert(format!("{:.3}", windows_price / linux_price));
        pairs += 1;
    }
    assert!(
        pairs >= 2,
        "case the whole catalog: expected the Windows tier to be published, got {ids:?}"
    );
    assert!(
        ratios.len() > 1,
        "case the whole catalog: each column is priced on its own, so the Windows prices are \
         not one markup over the Linux/Unix ones, got ratios {ratios:?}"
    );
}

#[tokio::test]
async fn the_price_a_plan_is_published_at_is_the_price_an_instance_on_it_is_billed() {
    let emulator = Emulator::new();

    // A plan the recorded answer prices, carrying a Windows image, created and
    // then costed through the operation that bills it: the catalog's figure and
    // the charge are one price, not two.
    let bundle_id = "micro_win_3_0";
    let name = "catalog-priced-windows-instance";
    emulator
        .ok(
            "CreateInstances",
            json!({
                "instanceNames": [name],
                "availabilityZone": "us-east-1a",
                "blueprintId": "windows_server_2025",
                "bundleId": bundle_id,
            }),
        )
        .await;

    let listed = listing(&emulator, "GetBundles", "bundles", json!({}))
        .await
        .into_iter()
        .find(|bundle| text(bundle, "bundleId") == bundle_id)
        .unwrap_or_else(|| panic!("case {bundle_id}: expected the catalog to publish this plan"));
    let published = listed["price"]
        .as_f64()
        .unwrap_or_else(|| panic!("case {bundle_id}: expected a numeric price, got {listed:?}"));

    let start = now_epoch_seconds() + WINDOW_STARTS_IN_SECONDS;
    let end = start + BILLING_MONTH_HOURS * 3_600.0;
    let answer = emulator
        .ok(
            "GetCostEstimate",
            json!({"resourceName": name, "startTime": start, "endTime": end}),
        )
        .await;
    let charge = answer["resourcesBudgetEstimate"][0]["costEstimates"]
        .as_array()
        .and_then(|charges| {
            charges
                .iter()
                .find(|charge| charge["usageType"] == json!(format!("BundleUsage:{bundle_id}")))
                .cloned()
        })
        .unwrap_or_else(|| {
            panic!(
                "case {bundle_id}: expected the plan's own usage type to be charged, got \
                 {answer:?}"
            )
        });
    let cost = charge["resultsByTime"][0]["usageCost"].as_f64();
    assert!(
        is_price(cost, published),
        "case {bundle_id}: expected one billing month to cost the published {published}, \
         got {cost:?}"
    );
}

// ---------------------------------------------------------------------------
// GetBlueprints — each image's own release, family token and links
// ---------------------------------------------------------------------------

/// `Blueprint$version` of the images this catalog publishes by BUILD DATE.
///
/// SOURCE: the recorded answer, which reports this one version on a Windows
/// Server operating-system image and on a Windows SQL Server application image —
/// the Windows family and the preinstalled stacks are rebuilt as a set and
/// published by that build's date.
const IMAGE_BUILD_VERSION: &str = "2026.09.17";

/// One published image and the release of the software it installs, as
/// `Blueprint$version` reports it ("The version number of the operating system,
/// application, or stack").
///
/// SOURCE: the versions the recorded `GetBlueprints` answer reports — the build
/// date above on the Windows family, and the Amazon Linux 2023 release string
/// `2023.12.20260930.0` — with the remaining operating systems carrying the
/// release their own id names.
///
/// The rows span what the rule generalises over: an image published by build date
/// and images published by their own release, an operating-system image and an
/// application image, and two releases of one family.
const IMAGE_RELEASES: &[(&str, &str)] = &[
    ("windows_server_2025", IMAGE_BUILD_VERSION),
    ("windows_server_2022_sql_2022_express", IMAGE_BUILD_VERSION),
    ("amazon_linux_2023", "2023.12.20260930.0"),
    ("amazon_linux_2", "2.0.20260930.0"),
    ("ubuntu_24_04", "24.04"),
    ("ubuntu_22_04", "22.04"),
    ("debian_12", "12"),
    ("freebsd_14", "14"),
];

/// Two releases of one image family, each a published entry of its own: their
/// group tokens may not be shared, because a group names one release.
const SIBLING_RELEASES: &[(&str, &str)] = &[
    ("amazon_linux_2023", "amazon_linux_2"),
    ("ubuntu_24_04", "ubuntu_22_04"),
    ("debian_12", "debian_11"),
    ("windows_server_2025", "windows_server_2022"),
];

/// Two releases of one family the catalog publishes BY RELEASE: their versions
/// may not be shared either, because each installs its own release of the
/// software. The Windows family is published by build date instead, so its
/// entries share that date and appear above only in [`SIBLING_RELEASES`].
const SIBLING_SOFTWARE_RELEASES: &[(&str, &str)] = &[
    ("amazon_linux_2023", "amazon_linux_2"),
    ("ubuntu_24_04", "ubuntu_22_04"),
    ("debian_12", "debian_11"),
];

#[tokio::test]
async fn every_published_image_reports_the_release_of_the_software_it_installs() {
    let emulator = Emulator::new();
    let catalog = blueprint_catalog(&emulator).await;

    for (blueprint_id, version) in IMAGE_RELEASES {
        let reported = image_of(&catalog, blueprint_id);
        assert_eq!(
            text(&reported, "version"),
            *version,
            "case {blueprint_id}: expected the release of the software this image installs, \
             {version:?}, got {reported:?}"
        );
    }

    // Catalog-wide: a version is the ENTRY's, so the listing carries more than
    // one, no two releases of a family report the same one, and every entry
    // carries a published release.
    let versions: BTreeSet<String> = catalog
        .iter()
        .map(|blueprint| text(blueprint, "version"))
        .collect();
    assert!(
        versions.len() > 1,
        "case the whole catalog: version is each image's own release, so the listing carries \
         more than one, got {versions:?}"
    );
    for (newer, older) in SIBLING_SOFTWARE_RELEASES {
        let (newer_version, older_version) = (
            text(&image_of(&catalog, newer), "version"),
            text(&image_of(&catalog, older), "version"),
        );
        assert_ne!(
            newer_version, older_version,
            "case {newer} vs {older}: two releases of one family install different software, \
             so they report different versions"
        );
    }
    for blueprint in &catalog {
        let case = text(blueprint, "blueprintId");
        let version = text(blueprint, "version");
        assert!(
            !version.is_empty() && version.chars().any(|c| c.is_ascii_digit()),
            "case {case}: expected a published version, got {version:?}"
        );
    }
}

/// One published image and the family token its release is grouped under
/// (`Blueprint$group`, "The group name of the blueprint").
///
/// SOURCE: the groups the recorded `GetBlueprints` answer reports —
/// `windows_2025` for the Windows Server 2025 image and `amazon_linux_2023` for
/// the Amazon Linux 2023 one — so a group names the image's RELEASE, which is why
/// the releases of one family carry different tokens.
///
/// The rows span an operating-system image whose token drops a word of its id,
/// operating systems whose token is their id, an application image preinstalling
/// a product on another image, and an application stack.
const IMAGE_GROUPS: &[(&str, &str)] = &[
    ("windows_server_2025", "windows_2025"),
    (
        "windows_server_2022_sql_2022_express",
        "windows_2022_sql_exp_2022",
    ),
    ("amazon_linux_2023", "amazon_linux_2023"),
    ("amazon_linux_2", "amazon_linux_2"),
    ("ubuntu_24_04", "ubuntu_24_04"),
    ("debian_10", "debian_10"),
    ("wordpress", "wordpress"),
];

#[tokio::test]
async fn every_published_image_is_grouped_under_the_family_token_of_its_release() {
    let emulator = Emulator::new();
    let catalog = blueprint_catalog(&emulator).await;

    for (blueprint_id, group) in IMAGE_GROUPS {
        let reported = image_of(&catalog, blueprint_id);
        assert_eq!(
            text(&reported, "group"),
            *group,
            "case {blueprint_id}: expected the family token of this release, {group:?}, \
             got {reported:?}"
        );
    }

    // Catalog-wide: an entry IS one release, so no two entries are grouped under
    // one token — a caller collating by `group` reaches the release it asked for
    // rather than a sibling.
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for blueprint in &catalog {
        let case = text(blueprint, "blueprintId");
        let group = text(blueprint, "group");
        assert!(
            !group.is_empty(),
            "case {case}: expected the published family token, got {blueprint:?}"
        );
        if let Some(other) = seen.insert(group.clone(), case.clone()) {
            panic!("case {case}: a group names one release, so {other} must not share {group:?}");
        }
    }
    for (newer, older) in SIBLING_RELEASES {
        assert_ne!(
            text(&image_of(&catalog, newer), "group"),
            text(&image_of(&catalog, older), "group"),
            "case {newer} vs {older}: two releases of one family are grouped apart"
        );
    }
}

/// One published image and the AWS page the catalog links it to, where a recorded
/// answer names that page exactly; `None` where the row only has to carry a
/// Marketplace listing of its own.
///
/// SOURCE: the `productUrl` values the recorded answer reports — a Marketplace
/// listing for the images AWS offers through one, and the product page
/// `aws.amazon.com/linux/amazon-linux-2023` for the Amazon Linux 2023 image, an
/// operating system AWS publishes its own page for.
const IMAGE_PAGES: &[(&str, Option<&str>)] = &[
    (
        "amazon_linux_2023",
        Some("https://aws.amazon.com/linux/amazon-linux-2023"),
    ),
    (
        "amazon_linux_2",
        Some("https://aws.amazon.com/amazon-linux-2"),
    ),
    ("windows_server_2025", None),
    ("wordpress", None),
    ("debian_10", None),
];

#[tokio::test]
async fn every_published_image_links_to_the_aws_page_published_for_it() {
    let emulator = Emulator::new();
    let catalog = blueprint_catalog(&emulator).await;

    for (blueprint_id, page) in IMAGE_PAGES {
        let reported = image_of(&catalog, blueprint_id);
        let product_url = text(&reported, "productUrl");
        match page {
            Some(page) => assert_eq!(
                product_url, *page,
                "case {blueprint_id}: expected the AWS page published for this image, \
                 got {reported:?}"
            ),
            None => assert!(
                product_url
                    .starts_with(&format!("{AWS_PRODUCT_PAGE_PREFIX}{MARKETPLACE_LISTING_PATH}")),
                "case {blueprint_id}: expected this image's own AWS Marketplace listing, got \
                 {product_url:?}"
            ),
        }
    }
}

/// The sentences AWS's catalog paragraph for a Windows Server image carries
/// beside the release it names, from the recorded answer's text for the Windows
/// Server 2025 image.
const WINDOWS_TEXT: &[&str] = &[
    "Lightsail runs Windows-based solutions in a secure, easily managed, and performant cloud \
     environment.",
    "running any other application requiring Windows software.",
];

/// The releases the Windows Server operating-system images install, each of which
/// its own paragraph names.
const WINDOWS_RELEASES: &[(&str, &str)] = &[
    ("windows_server_2025", "Windows Server 2025"),
    ("windows_server_2022", "Windows Server 2022"),
    ("windows_server_2019", "Windows Server 2019"),
    ("windows_server_2016", "Windows Server 2016"),
];

/// Wording AWS's own paragraph for the Amazon Linux images carries, which a
/// sentence written about the same image from its id and type does not.
const AMAZON_LINUX_TEXT: &[(&str, &[&str])] = &[
    (
        "amazon_linux_2023",
        &[
            "modern, general purpose Linux-based OS",
            "5 years of long term support",
            "optimized for AWS",
        ],
    ),
    (
        "amazon_linux_2",
        &["previous generation Amazon Linux", "long term support"],
    ),
];

#[tokio::test]
async fn the_catalog_text_of_an_image_is_the_paragraph_aws_publishes_for_it() {
    let emulator = Emulator::new();
    let catalog = blueprint_catalog(&emulator).await;

    // The Windows family shares AWS's paragraph, each entry naming the release it
    // installs — so the text is AWS's own and still tells the releases apart.
    for (blueprint_id, release) in WINDOWS_RELEASES {
        let description = text(&image_of(&catalog, blueprint_id), "description");
        assert!(
            description.contains(release),
            "case {blueprint_id}: expected the paragraph to name {release:?}, got {description:?}"
        );
        for sentence in WINDOWS_TEXT {
            assert!(
                description.contains(sentence),
                "case {blueprint_id}: expected AWS's wording {sentence:?}, got {description:?}"
            );
        }
    }

    for (blueprint_id, phrases) in AMAZON_LINUX_TEXT {
        let description = text(&image_of(&catalog, blueprint_id), "description");
        for phrase in *phrases {
            assert!(
                description.contains(phrase),
                "case {blueprint_id}: expected AWS's wording {phrase:?}, got {description:?}"
            );
        }
    }
}
