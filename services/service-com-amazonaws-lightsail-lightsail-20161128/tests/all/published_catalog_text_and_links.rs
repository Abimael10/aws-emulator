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
//!   the links THIS service serves for the image: its AWS Marketplace listing in
//!   `productUrl` and, where the image is offered under an agreement of its own,
//!   the end-user licence agreement AWS publishes for it in `licenseUrl` — not a
//!   sentence built from the entry's name and `type`, and not the upstream
//!   vendor's own product or licensing page.
//! * the blueprint catalog publishes the Windows SQL Server application images
//!   as a tier of their own, after the Windows operating-system images and before
//!   the first Linux/Unix image, each carrying the plan power the database engine
//!   needs.
//! * `GetBundles` reports each plan's `diskSizeInGb`, `transferPerMonthInGb` and
//!   `power` from the plan table's column for the plan's platform, so the columns
//!   stop at the figures the table stops at instead of growing with the plan's
//!   hardware, and the Windows plan of a size carries that column's own figures.
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

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::instance_lifecycle::Emulator;

/// The two values of the modelled `InstancePlatform` enum
/// (`smithy_query(query="shape",shape="InstancePlatform")`).
const LINUX_UNIX: &str = "LINUX_UNIX";
const WINDOWS: &str = "WINDOWS";

/// Where a published blueprint's `productUrl` leads: the AWS Marketplace product
/// view the image's listing is served at.
///
/// SOURCE: the `productUrl` values the recorded `GetBlueprints` answer reports,
/// which are all of the form `aws.amazon.com/marketplace/pp/prodview-<listing>`.
/// Written out here rather than read off the crate, so this suite pins the host
/// and the path form.
const MARKETPLACE_PRODUCT_VIEW_PREFIX: &str = "https://aws.amazon.com/marketplace/pp/prodview-";

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
/// `publishes_license` is whether the image is offered under an agreement of its
/// own, which a licensed operating system and a packaged application image are
/// and an openly licensed operating system is not.
///
/// The rows span what the rule generalises over: an operating-system image and
/// an application image, a Windows image and Linux/Unix ones, an image the
/// catalog names after the product it installs, a retired (inactive) entry, and
/// both an image offered under its own agreement and images offered under none.
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
        publishes_license: false,
    },
    Image {
        blueprint_id: "freebsd_14",
        names: "FreeBSD 14",
        publishes_license: false,
    },
    // A retired entry is still an entry: it carries the same columns, which is
    // what a caller reading an existing instance's image needs.
    Image {
        blueprint_id: "debian_10",
        names: "Debian 10",
        publishes_license: false,
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
            product_url.starts_with(MARKETPLACE_PRODUCT_VIEW_PREFIX)
                && product_url.len() > MARKETPLACE_PRODUCT_VIEW_PREFIX.len(),
            "case {case}: productUrl must be this image's listing on the service's own \
             Marketplace, got {product_url:?}"
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

    // Catalog-wide: every link a published entry carries is served by THIS
    // service — the listing on its own Marketplace, the agreement on its own
    // licence host — and no two entries point at one listing.
    let mut listings: BTreeSet<String> = BTreeSet::new();
    for blueprint in &blueprints {
        let case = text(blueprint, "blueprintId");
        let product_url = text(blueprint, "productUrl");
        assert!(
            product_url.starts_with(MARKETPLACE_PRODUCT_VIEW_PREFIX),
            "case {case}: productUrl must be under {MARKETPLACE_PRODUCT_VIEW_PREFIX}, got \
             {product_url:?}"
        );
        assert!(
            listings.insert(product_url.clone()),
            "case {case}: a listing is the entry's own, so no two entries share one, got \
             {product_url:?}"
        );
        if let Some(license_url) = blueprint["licenseUrl"].as_str() {
            assert!(
                license_url.starts_with(LICENSE_AGREEMENT_PREFIX),
                "case {case}: licenseUrl must be under {LICENSE_AGREEMENT_PREFIX}, got \
                 {license_url:?}"
            );
        }
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
