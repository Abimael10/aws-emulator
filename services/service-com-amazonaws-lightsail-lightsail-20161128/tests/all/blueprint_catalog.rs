// SPDX-License-Identifier: CC0-1.0

//! `GetBlueprints` — the instance-image (blueprint) catalog, driven over the wire.
//!
//! Every wire case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: Lightsail_20161128.GetBlueprints` and an `awsJson1_1` body,
//! dispatched by the multi-service router, so the modelled-constraint walk, the
//! handler and the awsJson error envelope are all exercised rather than bypassed.
//!
//! The rules under test, asserted over the WHOLE catalog and over sets of
//! requests rather than one recorded call:
//!
//! * every reported blueprint's members are its catalog entry's: the friendly
//!   `name` is the catalog's own name for the image (the id title-cased where the
//!   id's words spell it, the product's name where the catalog names the image
//!   after the product it installs — the same name `Instance$blueprintName`
//!   reports, so an instance created from the blueprint carries it too), `group`
//!   is the family name the catalog publishes for the entry's release, `version`
//!   is the release of the software the entry's image installs and is therefore
//!   the entry's own, `platform` is `WINDOWS` for exactly the Windows images and
//!   `LINUX_UNIX` otherwise, `minPower` is the power of the smallest plan the
//!   image runs on (the floor the entry publishes, else its platform's: the
//!   `micro` plan's for a Windows image, 0 — "the blueprint runs on all instance
//!   sizes" — for the others), `versionCode` is the published revision of the
//!   image, and a vendor link the catalog publishes is reported as an absolute
//!   `https` URL;
//! * `includeInactive` is what widens the answer from the active entries to every
//!   published one, and an absent flag reports the active ones only;
//! * `appCategory` selects the catalog: a request naming a category reports the
//!   Lightsail for Research blueprints (this emulator publishes none, so that
//!   list is empty), one naming none reports the instance blueprints, and a
//!   category outside the modelled `AppCategory` enum is refused by the
//!   constraint walk with a 400 `ValidationException`;
//! * a page is the reported list from the entry `pageToken` names onward, bounded
//!   by the page size, with `nextPageToken` present only while entries remain —
//!   so the walk reports each entry in order, exactly once — and a token that
//!   names no reported blueprint is a modelled 400 `InvalidInputException`;
//! * only an ACTIVE blueprint can be created from, and the instance then reports
//!   the catalog's own name for it; and
//! * the operation reports, never mutates: identical calls answer identically.
//!
//! The paging and slicing rules are additionally exercised directly against
//! `state::blueprints_from` / `state::blueprint_page` over SYNTHETIC catalogs, at
//! page sizes the published catalog (answered whole in one page) cannot reach.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use service_com_amazonaws_lightsail_lightsail_20161128::state;

// The front door every suite in this test target drives the service through, and
// the awsJson error-code reader, both already implemented beside this suite.
use super::instance_lifecycle::{error_code, Emulator};

/// The operation under test.
const OPERATION: &str = "GetBlueprints";

/// The page size the service publishes for this operation, read from the crate
/// rather than restated here, so a page that grows stays bounded by it.
const PAGE_SIZE: usize = state::BLUEPRINT_PAGE_SIZE;

/// The two values of the modelled `BlueprintType` enum
/// (`smithy_query(query="shape",shape="BlueprintType")`).
const BLUEPRINT_TYPES: &[&str] = &["os", "app"];

/// The two values of the modelled `InstancePlatform` enum
/// (`smithy_query(query="shape",shape="InstancePlatform")`).
const PLATFORMS: &[&str] = &["LINUX_UNIX", "WINDOWS"];

/// Where a published blueprint's `productUrl` leads: the AWS site, which serves
/// both the Marketplace listing an image is offered through and the product page
/// AWS keeps for an operating system of its own. Written out here rather than
/// read off the crate, so the host is pinned by this suite.
const AWS_PRODUCT_PAGE_PREFIX: &str = "https://aws.amazon.com/";

/// Where a published blueprint's `licenseUrl` leads when AWS serves the
/// agreement as a document of its own: the AWS licence host. An image whose terms
/// AWS publishes on its Marketplace listing instead links that listing's usage
/// section ([`LISTING_USAGE_TERMS_FRAGMENT`]).
const LICENSE_AGREEMENT_PREFIX: &str = "https://d7umqicpi7263.cloudfront.net/eula/product/";

/// The section of a Marketplace listing that carries an image's usage terms,
/// which `licenseUrl` names where AWS publishes the terms on the listing.
const LISTING_USAGE_TERMS_FRAGMENT: &str = "#pdp-usage";

/// One expected catalog entry.
///
/// Source: the blueprint ids this service publishes (`state::BLUEPRINTS`, whose
/// own source is the Lightsail instance-image list), chosen to span the class the
/// rules generalise over: an operating system and an application stack, Windows
/// images and Linux/Unix ones, an image the catalog names after its id's words
/// and one it names after the product it installs, an image that runs on every
/// plan and one that publishes a higher floor, a retired (inactive) entry, and
/// both an entry whose vendor publishes product and licence links and entries
/// that publish neither.
struct Expected {
    blueprint_id: &'static str,
    blueprint_type: &'static str,
    name: &'static str,
    group: &'static str,
    platform: &'static str,
    is_active: bool,
    /// `Blueprint$minPower`: the power of the smallest instance plan the image
    /// runs on.
    min_power: i64,
    /// Whether the catalog publishes an AWS page describing this image, and
    /// whether it publishes the end-user licence agreement AWS serves for it —
    /// which every image the catalog offers carries, a recorded Amazon Linux
    /// entry included.
    publishes_product_url: bool,
    publishes_license_url: bool,
}

const EXPECTED: &[Expected] = &[
    Expected {
        blueprint_id: "windows_server_2025",
        blueprint_type: "os",
        name: "Windows Server 2025",
        group: "windows_2025",
        platform: "WINDOWS",
        is_active: true,
        min_power: 500,
        publishes_product_url: true,
        publishes_license_url: true,
    },
    Expected {
        blueprint_id: "windows_server_2022_sql_2022_express",
        blueprint_type: "app",
        name: "SQL Server 2022 Express",
        group: "windows_2022_sql_exp_2022",
        platform: "WINDOWS",
        is_active: true,
        min_power: 2_000,
        publishes_product_url: true,
        publishes_license_url: true,
    },
    Expected {
        blueprint_id: "windows_server_2022_sql_2019_express",
        blueprint_type: "app",
        name: "SQL Server 2019 Express",
        group: "windows_2022_sql_exp_2019",
        platform: "WINDOWS",
        is_active: true,
        min_power: 2_000,
        publishes_product_url: true,
        publishes_license_url: true,
    },
    Expected {
        blueprint_id: "amazon_linux_2023",
        blueprint_type: "os",
        name: "Amazon Linux 2023",
        group: "amazon_linux_2023",
        platform: "LINUX_UNIX",
        is_active: true,
        min_power: 0,
        publishes_product_url: true,
        publishes_license_url: true,
    },
    Expected {
        blueprint_id: "centos_stream_9",
        blueprint_type: "os",
        name: "Centos Stream 9",
        group: "centos_stream_9",
        platform: "LINUX_UNIX",
        is_active: true,
        min_power: 0,
        publishes_product_url: true,
        publishes_license_url: true,
    },
    Expected {
        blueprint_id: "ubuntu_22_04",
        blueprint_type: "os",
        name: "Ubuntu 22 04",
        group: "ubuntu_22_04",
        platform: "LINUX_UNIX",
        is_active: true,
        min_power: 0,
        publishes_product_url: true,
        publishes_license_url: true,
    },
    Expected {
        blueprint_id: "ubuntu_18_04",
        blueprint_type: "os",
        name: "Ubuntu 18 04",
        group: "ubuntu_18_04",
        platform: "LINUX_UNIX",
        is_active: false,
        min_power: 0,
        publishes_product_url: true,
        publishes_license_url: true,
    },
    Expected {
        blueprint_id: "debian_10",
        blueprint_type: "os",
        name: "Debian 10",
        group: "debian_10",
        platform: "LINUX_UNIX",
        is_active: false,
        min_power: 0,
        publishes_product_url: true,
        publishes_license_url: true,
    },
    Expected {
        blueprint_id: "windows_server_2019",
        blueprint_type: "os",
        name: "Windows Server 2019",
        group: "windows_2019",
        platform: "WINDOWS",
        is_active: true,
        min_power: 500,
        publishes_product_url: true,
        publishes_license_url: true,
    },
    Expected {
        blueprint_id: "wordpress",
        blueprint_type: "app",
        name: "Wordpress",
        group: "wordpress",
        platform: "LINUX_UNIX",
        is_active: true,
        min_power: 0,
        publishes_product_url: true,
        publishes_license_url: true,
    },
    Expected {
        blueprint_id: "lamp_8_bitnami",
        blueprint_type: "app",
        name: "Lamp 8 Bitnami",
        group: "lamp-bitnami",
        platform: "LINUX_UNIX",
        is_active: true,
        min_power: 0,
        publishes_product_url: true,
        publishes_license_url: true,
    },
    Expected {
        blueprint_id: "plesk_ubuntu_18_0_35",
        blueprint_type: "app",
        name: "Plesk Ubuntu 18 0 35",
        group: "plesk-ubuntu",
        platform: "LINUX_UNIX",
        is_active: true,
        min_power: 0,
        publishes_product_url: true,
        publishes_license_url: true,
    },
    Expected {
        blueprint_id: "cpanel_whm_almalinux",
        blueprint_type: "app",
        name: "Cpanel Whm Almalinux",
        group: "cpanel-whm-almalinux",
        platform: "LINUX_UNIX",
        is_active: true,
        min_power: 0,
        publishes_product_url: true,
        publishes_license_url: true,
    },
];

// ---------------------------------------------------------------------------
// The derivations, restated here as the rule rather than read off the crate
// ---------------------------------------------------------------------------

/// The powers the published plan catalog offers, which a blueprint's minimum is
/// one of (or 0 — "the blueprint runs on all instance sizes").
fn published_powers() -> Vec<i64> {
    state::BUNDLE_SPECS
        .iter()
        .map(|spec| i64::from(spec.power))
        .collect()
}

/// The power of the `micro` plan: the floor a Windows image cannot go below.
fn micro_power() -> i64 {
    i64::from(
        state::BUNDLE_SPECS
            .iter()
            .find(|spec| spec.size == "micro")
            .expect("the published plan catalog carries the micro plan")
            .power,
    )
}

/// Whether `version` is a published software version: a dotted numeric release
/// string, which is the form `Blueprint$version` carries (its own example,
/// `2016.03.0`, is that form) — the release of the operating system, application
/// or stack the image installs, or the date of the build the catalog publishes
/// the image by.
fn is_published_version(version: &str) -> bool {
    let components: Vec<&str> = version.split('.').collect();
    !components.is_empty()
        && components
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

/// The friendly name of a blueprint the catalog names after its id: the id's
/// words title-cased, space-joined. An image the catalog names after the product
/// it installs carries that product's name instead, so this is the form a name
/// takes only where the id's words spell it.
fn want_name(blueprint_id: &str) -> String {
    blueprint_id
        .split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The platform of a blueprint id: Windows for exactly the Windows images.
fn want_platform(blueprint_id: &str) -> &'static str {
    if blueprint_id.starts_with("windows") {
        "WINDOWS"
    } else {
        "LINUX_UNIX"
    }
}

/// The minimum bundle power an image of `platform` requires, where the catalog
/// publishes no floor of its own for the image: a Windows image does not run on
/// the smallest plan, so its floor is the power of the `micro` plan; every other
/// published image runs on all sizes, which 0 states.
fn want_min_power(platform: &str) -> i64 {
    if platform == "WINDOWS" {
        micro_power()
    } else {
        0
    }
}

/// The ids the published catalog holds, in its own order, filtered the way
/// `includeInactive` filters.
fn published_ids(include_inactive: bool) -> Vec<String> {
    state::BLUEPRINTS
        .iter()
        .filter(|spec| include_inactive || spec.is_active)
        .map(|spec| spec.blueprint_id.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// This suite's reads, over the shared harness
// ---------------------------------------------------------------------------

/// One page of the catalog: the page's blueprints and its `nextPageToken`.
async fn page(emulator: &Emulator, request: Value) -> (Vec<Value>, Option<String>) {
    let case = request.clone();
    let body = emulator.ok(OPERATION, request).await;
    let blueprints = body["blueprints"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a blueprints array, got {body:?}"))
        .clone();
    let next = body["nextPageToken"].as_str().map(str::to_string);
    (blueprints, next)
}

/// Every blueprint the walk from `request` reports, in page order, together with
/// the size of each page it took.
async fn walk(emulator: &Emulator, request: Value) -> (Vec<Value>, Vec<usize>) {
    let mut blueprints = Vec::new();
    let mut page_sizes = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut body = request.clone();
        if let Some(token) = &token {
            body["pageToken"] = json!(token);
        }
        let (reported, next) = page(emulator, body).await;
        page_sizes.push(reported.len());
        blueprints.extend(reported);
        match next {
            Some(next) => token = Some(next),
            None => break,
        }
        assert!(
            page_sizes.len() < 100,
            "case {request:?}: the walk did not terminate after {} pages",
            page_sizes.len()
        );
    }
    (blueprints, page_sizes)
}

/// A member of a blueprint, as a string, or a panic naming the case.
fn member(blueprint: &Value, name: &str) -> String {
    blueprint[name]
        .as_str()
        .unwrap_or_else(|| panic!("case {blueprint:?}: expected a string {name}"))
        .to_string()
}

/// The reported ids of a list of blueprints.
fn ids(blueprints: &[Value]) -> Vec<String> {
    blueprints
        .iter()
        .map(|blueprint| member(blueprint, "blueprintId"))
        .collect()
}

// ---------------------------------------------------------------------------
// The reported members
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_reported_blueprint_derives_its_members_from_its_catalog_entry() {
    let emulator = Emulator::new();
    let (blueprints, _) = walk(&emulator, json!({"includeInactive": true})).await;

    assert!(
        !blueprints.is_empty(),
        "case whole catalog: expected at least one blueprint, got none"
    );

    for blueprint in &blueprints {
        let case = member(blueprint, "blueprintId");
        assert!(
            !case.is_empty(),
            "case {blueprint:?}: expected a non-empty blueprintId"
        );

        let blueprint_type = member(blueprint, "type");
        assert!(
            BLUEPRINT_TYPES.contains(&blueprint_type.as_str()),
            "case {case:?}: type must be one of {BLUEPRINT_TYPES:?}, got {blueprint_type:?}"
        );

        // The catalog's own name for the image: the id's words title-cased where
        // the id spells the name, the product's name where the catalog names the
        // image after the product it installs — never the raw id.
        let name = member(blueprint, "name");
        assert!(
            !name.is_empty() && !name.contains('_'),
            "case {case:?}: name must be the catalog's own name for the image, got {blueprint:?}"
        );
        if name != want_name(&case) {
            assert!(
                EXPECTED.iter().any(|want| want.blueprint_id == case),
                "case {case:?}: an image the catalog names after its product must be pinned by \
                 name in this suite's table, got {name:?}"
            );
        }
        assert!(
            !member(blueprint, "group").is_empty(),
            "case {case:?}: group must name the entry's published family, got {blueprint:?}"
        );
        // `version` is the release of the software THIS image installs, so it is
        // the entry's own: an image of a named release reports that release
        // rather than one figure shared by the listing.
        let version = member(blueprint, "version");
        assert!(
            is_published_version(&version),
            "case {case:?}: version must be the published release of the image's software, \
             got {blueprint:?}"
        );

        let platform = member(blueprint, "platform");
        assert!(
            PLATFORMS.contains(&platform.as_str()),
            "case {case:?}: platform must be one of {PLATFORMS:?}, got {platform:?}"
        );
        assert_eq!(
            platform,
            want_platform(&case),
            "case {case:?}: Windows is the platform of exactly the Windows images, \
             got {blueprint:?}"
        );

        assert!(
            blueprint["isActive"].is_boolean(),
            "case {case:?}: expected a boolean isActive, got {blueprint:?}"
        );
        // The minimum is a power the plan catalog offers — the floor the image's
        // own entry publishes, or its platform's — so it names a plan a caller
        // can actually select, and a Windows image never reports below the
        // smallest plan Windows Server runs on.
        let min_power = blueprint["minPower"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case:?}: expected a numeric minPower"));
        assert!(
            min_power == 0 || published_powers().contains(&min_power),
            "case {case:?}: minPower must be the power of a published plan, got {min_power}"
        );
        assert!(
            min_power >= want_min_power(&platform),
            "case {case:?}: expected at least the floor of {platform}, got {min_power}"
        );

        // The description is the catalog's own text for the entry, carried per
        // row: it is not a sentence built from the entry's name and type, which
        // would tell a caller nothing the other members do not already say.
        let description = member(blueprint, "description");
        assert!(
            description.len() > 80,
            "case {case:?}: expected the catalog's published text for the image, got \
             {description:?}"
        );
        for derived in [
            format!("{name} operating system image for a Lightsail instance."),
            format!("{name} application stack, preinstalled on a Lightsail instance."),
        ] {
            assert_ne!(
                description, derived,
                "case {case:?}: the description must be the catalog's text, not one derived \
                 from the entry's name and type"
            );
        }
        // And the text is about THIS image: the first word of the catalog's own
        // name for the image appears in it. Both sides are compared as their
        // alphanumeric characters alone, since the catalog's prose may spell a
        // product's name with punctuation the entry's name does not carry
        // (`Node.js` for the `nodejs` entry).
        let squashed = |text: &str| -> String {
            text.chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect()
        };
        let product = name.split_whitespace().next().unwrap_or_default();
        assert!(
            squashed(&description).contains(&squashed(product)),
            "case {case:?}: expected the catalog's text for this image to be about \
             {product:?}, got {description:?}"
        );

        // The image's published revision is reported, and a link the catalog
        // publishes leads to the page THIS service serves for the image: its AWS
        // Marketplace listing and the end-user licence agreement AWS publishes
        // for it, never the upstream vendor's own product or licensing page.
        // `appCategory` belongs to the Research catalog alone, so this listing
        // reports none.
        assert!(
            !member(blueprint, "versionCode").is_empty(),
            "case {case:?}: expected the published versionCode, got {blueprint:?}"
        );
        if let Some(url) = blueprint["productUrl"].as_str() {
            assert!(
                url.starts_with(AWS_PRODUCT_PAGE_PREFIX)
                    && url.len() > AWS_PRODUCT_PAGE_PREFIX.len(),
                "case {case:?}: productUrl must be the AWS page this service serves for the \
                 image, got {url:?}"
            );
        }
        // The licence link leads to the agreement AWS publishes for the image, in
        // whichever of the two published forms carries it: the agreement document
        // on the AWS licence host, or the usage section of THIS image's own
        // Marketplace listing.
        if let Some(url) = blueprint["licenseUrl"].as_str() {
            let on_license_host = url.starts_with(LICENSE_AGREEMENT_PREFIX)
                && url.len() > LICENSE_AGREEMENT_PREFIX.len();
            let own_listing_terms = blueprint["productUrl"]
                .as_str()
                .is_some_and(|page| url == format!("{page}{LISTING_USAGE_TERMS_FRAGMENT}"));
            assert!(
                on_license_host || own_listing_terms,
                "case {case:?}: licenseUrl must be the agreement AWS publishes for this image \
                 — a document under {LICENSE_AGREEMENT_PREFIX}… or this image's own listing \
                 with {LISTING_USAGE_TERMS_FRAGMENT} — got {url:?}"
            );
        }
        assert!(
            blueprint["appCategory"].is_null(),
            "case {case:?}: appCategory is the Research catalog's, so it must be absent, \
             got {blueprint:?}"
        );
    }
}

#[tokio::test]
async fn the_published_sample_entries_report_their_expected_members() {
    let emulator = Emulator::new();
    let (blueprints, _) = walk(&emulator, json!({"includeInactive": true})).await;
    let reported = ids(&blueprints);

    for want in EXPECTED {
        let case = want.blueprint_id;
        let found = blueprints
            .iter()
            .find(|blueprint| member(blueprint, "blueprintId") == case)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the catalog to carry it, got {reported:?}")
            });

        assert_eq!(
            member(found, "type"),
            want.blueprint_type,
            "case {case:?}: wrong type, got {found:?}"
        );
        assert_eq!(
            member(found, "name"),
            want.name,
            "case {case:?}: wrong name, got {found:?}"
        );
        assert_eq!(
            member(found, "group"),
            want.group,
            "case {case:?}: wrong group, got {found:?}"
        );
        assert_eq!(
            member(found, "platform"),
            want.platform,
            "case {case:?}: wrong platform, got {found:?}"
        );
        assert_eq!(
            found["isActive"],
            json!(want.is_active),
            "case {case:?}: wrong isActive, got {found:?}"
        );
        assert_eq!(
            found["minPower"],
            json!(want.min_power),
            "case {case:?}: wrong minPower, got {found:?}"
        );
        for (link, published) in [
            ("productUrl", want.publishes_product_url),
            ("licenseUrl", want.publishes_license_url),
        ] {
            assert_eq!(
                found[link].is_string(),
                published,
                "case {case:?}: wrong {link} publication, got {found:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// includeInactive
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inactive_blueprints_are_reported_only_when_the_request_asks_for_them() {
    let emulator = Emulator::new();

    // Rows: the flag absent, sent false, and sent true.
    for (request, include_inactive) in [
        (json!({}), false),
        (json!({"includeInactive": false}), false),
        (json!({"includeInactive": true}), true),
    ] {
        let case = request.clone();
        let (blueprints, _) = walk(&emulator, request).await;
        let reported = ids(&blueprints);

        assert_eq!(
            reported,
            published_ids(include_inactive),
            "case {case:?}: expected the published catalog in its own order, got {reported:?}"
        );
        if !include_inactive {
            for blueprint in &blueprints {
                assert_eq!(
                    blueprint["isActive"],
                    json!(true),
                    "case {case:?}: an answer that did not ask for inactive blueprints \
                     carries active ones only, got {blueprint:?}"
                );
            }
        }
    }

    // The entries the flag adds are exactly the inactive ones, and they are
    // added where the catalog holds them rather than appended.
    let (active_only, _) = walk(&emulator, json!({})).await;
    let (everything, _) = walk(&emulator, json!({"includeInactive": true})).await;
    let active_ids = ids(&active_only);
    let all_ids = ids(&everything);
    let added: Vec<&String> = all_ids
        .iter()
        .filter(|id| !active_ids.contains(id))
        .collect();
    let inactive_published: Vec<String> = state::BLUEPRINTS
        .iter()
        .filter(|spec| !spec.is_active)
        .map(|spec| spec.blueprint_id.to_string())
        .collect();
    assert_eq!(
        added.len(),
        inactive_published.len(),
        "case includeInactive: expected the inactive entries {inactive_published:?} to be \
         what the flag adds, got {added:?}"
    );
    for id in &inactive_published {
        assert!(
            all_ids.contains(id),
            "case includeInactive: expected {id:?} among {all_ids:?}"
        );
        assert!(
            !active_ids.contains(id),
            "case default: expected {id:?} to be withheld, got {active_ids:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// appCategory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_request_naming_an_app_category_reports_the_research_catalog() {
    let emulator = Emulator::new();

    let (blueprints, next) = page(&emulator, json!({"appCategory": "LfR"})).await;
    // This emulator publishes no Lightsail for Research blueprint, so the
    // Research catalog is empty — and an empty list is still a list, with no
    // next page to walk to.
    assert!(
        blueprints.is_empty(),
        "case appCategory LfR: this emulator publishes no Research blueprint, so the \
         Research catalog is empty, got {blueprints:?}"
    );
    assert_eq!(
        blueprints.len(),
        state::LFR_BLUEPRINTS.len(),
        "case appCategory LfR: expected the published Research catalog, got {blueprints:?}"
    );
    assert_eq!(
        next, None,
        "case appCategory LfR: a last page carries no nextPageToken, got {next:?}"
    );

    // The category SELECTS the catalog rather than filtering the standard one, so
    // a request naming one can never report an instance blueprint: the two
    // catalogs the selection answers from are disjoint.
    let standard_catalog: Vec<&str> = state::blueprint_catalog(None)
        .iter()
        .map(|spec| spec.blueprint_id)
        .collect();
    let research_catalog: Vec<&str> = state::blueprint_catalog(Some("LfR"))
        .iter()
        .map(|spec| spec.blueprint_id)
        .collect();
    assert_eq!(
        standard_catalog,
        published_ids(true),
        "case appCategory absent: expected the instance blueprints, got {standard_catalog:?}"
    );
    for id in &research_catalog {
        assert!(
            !standard_catalog.contains(id),
            "case appCategory LfR: the Research catalog must not hold an instance \
             blueprint, got {id:?}"
        );
    }

    // And the instance catalog is unaffected by a Research read.
    let (standard, _) = walk(&emulator, json!({})).await;
    assert_eq!(
        ids(&standard),
        published_ids(false),
        "case after a Research read: the instance catalog must be unchanged"
    );
    for id in &ids(&blueprints) {
        assert!(
            !published_ids(true).contains(id),
            "case appCategory LfR: an instance blueprint must not be reported as a \
             Research one, got {id:?}"
        );
    }
}

#[tokio::test]
async fn an_app_category_outside_the_modelled_enum_is_refused() {
    let emulator = Emulator::new();

    // Rows differ along the ways a category can fail to name an `AppCategory`
    // member: wrong case, a longer form, an unrelated word, an empty string and
    // a padded value. The member carries the modelled enum's value set, so the
    // refusal is the constraint walk's `ValidationException`, before the handler.
    for category in ["lfr", "LFR", "LightsailForResearch", "", "LfR "] {
        let (status, body) = emulator
            .call(OPERATION, json!({"appCategory": category}))
            .await;
        assert_eq!(
            status, 400,
            "case appCategory {category:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case appCategory {category:?}: expected the modelled enum's value set to be \
             enforced, got {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Paging
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_page_token_resumes_the_catalog_at_the_blueprint_it_names() {
    let emulator = Emulator::new();

    for request in [json!({}), json!({"includeInactive": true})] {
        let case = request.clone();
        let (whole, page_sizes) = walk(&emulator, request.clone()).await;
        let all_ids = ids(&whole);

        // Each page is bounded by the published page size, and `nextPageToken` is
        // present exactly while entries remain: so every page but the last is
        // full, and the pages account for every entry.
        for (index, size) in page_sizes.iter().enumerate() {
            assert!(
                *size <= PAGE_SIZE,
                "case {case:?} page {index}: a page carries at most {PAGE_SIZE} blueprints, \
                 got {size}"
            );
            if index + 1 != page_sizes.len() {
                assert_eq!(
                    *size, PAGE_SIZE,
                    "case {case:?} page {index}: a page followed by another must be full, \
                     got {size}"
                );
            }
        }
        assert_eq!(
            all_ids.len(),
            page_sizes.iter().sum::<usize>(),
            "case {case:?}: the pages must account for every blueprint, got {page_sizes:?}"
        );
        let unique: BTreeSet<&String> = all_ids.iter().collect();
        assert_eq!(
            unique.len(),
            all_ids.len(),
            "case {case:?}: the walk reported a blueprint twice: {all_ids:?}"
        );

        // Every reported id is a token, including the first and the last: the
        // page it opens is the reported list from there on.
        for (index, token) in all_ids.iter().enumerate() {
            let mut body = request.clone();
            body["pageToken"] = json!(token);
            let (suffix, _) = walk(&emulator, body).await;
            assert_eq!(
                ids(&suffix),
                all_ids[index..],
                "case {case:?} pageToken {token:?}: expected the reported list from there on"
            );
        }

        // An absent token and an empty one both ask for the first page.
        for token in [None, Some("")] {
            let mut body = request.clone();
            if let Some(token) = token {
                body["pageToken"] = json!(token);
            }
            let (first_page, _) = page(&emulator, body).await;
            assert_eq!(
                ids(&first_page),
                all_ids[..page_sizes[0]],
                "case {case:?} pageToken {token:?}: expected the first page"
            );
        }
    }
}

#[tokio::test]
async fn a_page_token_that_names_no_reported_blueprint_is_refused() {
    let emulator = Emulator::new();

    // Rows differ along the ways a token can fail to name an entry of the
    // reported list: an id that is not published, a family without its version, a
    // version that is not served, the group form, whitespace, an opaque cursor
    // from elsewhere — and an id that IS published but is withheld from this
    // request, which the default request withholds because it is inactive.
    for token in [
        "not_a_blueprint",
        "ubuntu",
        "ubuntu_19_04",
        "amazon-linux",
        " ",
        "AAAAAAAA",
        "debian_10",
    ] {
        let (status, body) = emulator.call(OPERATION, json!({"pageToken": token})).await;
        assert_eq!(
            status, 400,
            "case pageToken {token:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidInputException",
            "case pageToken {token:?}: expected the modelled InvalidInputException, got {body:?}"
        );
    }

    // The same token, on a request that DOES report that blueprint, opens a page
    // at it: the token is resolved against the list the request asks for.
    let (resumed, _) = page(
        &emulator,
        json!({"pageToken": "debian_10", "includeInactive": true}),
    )
    .await;
    assert_eq!(
        ids(&resumed).first().map(String::as_str),
        Some("debian_10"),
        "case pageToken debian_10 with includeInactive: expected the page to open at it, \
         got {resumed:?}"
    );
}

// ---------------------------------------------------------------------------
// Paging and slicing over synthetic catalogs
// ---------------------------------------------------------------------------

/// A synthetic catalog: two active entries, a retired one between them and two
/// more, so a page boundary can fall either side of the inactive entry. One
/// entry publishes its own name and power floor, as the application images the
/// catalog names after their product do, so the slice carries both forms.
const SYNTHETIC: &[state::BlueprintSpec] = &[
    state::BlueprintSpec {
        blueprint_id: "synthetic_os_1",
        published_name: None,
        published_min_power: None,
        group: "synthetic-os",
        blueprint_type: state::BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "A synthetic operating-system image, for the slice and paging rules.",
        version: "1.0",
        product_page: "",
        license: state::BlueprintLicense::NotPublished,
    },
    state::BlueprintSpec {
        blueprint_id: "synthetic_os_2",
        published_name: None,
        published_min_power: None,
        group: "synthetic-os",
        blueprint_type: state::BLUEPRINT_TYPE_OS,
        is_active: false,
        description: "A synthetic retired operating-system image.",
        version: "2.0",
        product_page: "",
        license: state::BlueprintLicense::NotPublished,
    },
    state::BlueprintSpec {
        blueprint_id: "synthetic_app",
        published_name: Some("Synthetic Product 4 Standard"),
        published_min_power: Some(3_000),
        group: "synthetic-app",
        blueprint_type: state::BLUEPRINT_TYPE_APP,
        is_active: true,
        description: "A synthetic application image that publishes its own name, power floor, \
                      listing and licence agreement.",
        version: "4.0",
        product_page: "marketplace/pp/prodview-synthetic1",
        license: state::BlueprintLicense::Agreement("synthetic-product/synthetic-agreement.txt"),
    },
    state::BlueprintSpec {
        blueprint_id: "windows_synthetic_2030",
        published_name: None,
        published_min_power: None,
        group: "windows_2030",
        blueprint_type: state::BLUEPRINT_TYPE_OS,
        is_active: true,
        description: "A synthetic Windows operating-system image.",
        version: "2030.01.01",
        product_page: "",
        license: state::BlueprintLicense::NotPublished,
    },
    state::BlueprintSpec {
        blueprint_id: "synthetic_retired_3",
        published_name: None,
        published_min_power: None,
        group: "synthetic-retired",
        blueprint_type: state::BLUEPRINT_TYPE_APP,
        is_active: false,
        description: "A synthetic retired application image.",
        version: "3.0",
        product_page: "",
        license: state::BlueprintLicense::NotPublished,
    },
];

#[test]
fn the_slice_rule_holds_over_a_catalog_that_mixes_active_and_retired_entries() {
    for include_inactive in [false, true] {
        let reported = state::blueprints_from(SYNTHETIC, include_inactive, None);
        let want: Vec<&str> = SYNTHETIC
            .iter()
            .filter(|spec| include_inactive || spec.is_active)
            .map(|spec| spec.blueprint_id)
            .collect();
        let got: Vec<String> = reported
            .iter()
            .map(|blueprint| blueprint.blueprint_id.clone().unwrap_or_default())
            .collect();
        assert_eq!(
            got, want,
            "case includeInactive {include_inactive}: expected the catalog's own order, \
             got {got:?}"
        );
        for blueprint in &reported {
            assert_eq!(
                blueprint.app_category, None,
                "case includeInactive {include_inactive}: a standard blueprint carries no \
                 appCategory, got {blueprint:?}"
            );
        }
    }

    // A catalog read for a category stamps every entry with it: that is the
    // category of the catalog the entry came from.
    for blueprint in state::blueprints_from(SYNTHETIC, true, Some("LfR")) {
        assert_eq!(
            blueprint.app_category.as_deref(),
            Some("LfR"),
            "case appCategory LfR: expected the entry to carry its category, got {blueprint:?}"
        );
    }
}

#[test]
fn a_paged_walk_reports_every_entry_once_at_any_page_size() {
    let reported = state::blueprints_from(SYNTHETIC, true, None);
    let all_ids: Vec<String> = reported
        .iter()
        .map(|blueprint| blueprint.blueprint_id.clone().unwrap_or_default())
        .collect();

    // Rows: page sizes below, at and above the catalog's size, plus 0 — which
    // cannot mean "an empty page forever", so it carries one entry.
    for page_size in [0usize, 1, 2, 3, 5, 9] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let (page, next) = state::blueprint_page(&reported, page_size, token.as_deref())
                .unwrap_or_else(|| {
                    panic!("case page size {page_size}: token {token:?} must name an entry")
                });
            assert!(
                !page.is_empty(),
                "case page size {page_size}: a page over a non-empty catalog carries at \
                 least one entry"
            );
            assert!(
                page.len() <= page_size.max(1),
                "case page size {page_size}: a page carries at most that many entries, \
                 got {}",
                page.len()
            );
            walked.extend(
                page.iter()
                    .map(|blueprint| blueprint.blueprint_id.clone().unwrap_or_default()),
            );
            pages += 1;
            assert!(
                pages <= all_ids.len(),
                "case page size {page_size}: the walk did not terminate, got {walked:?}"
            );
            match next {
                Some(next) => token = Some(next),
                None => break,
            }
        }
        assert_eq!(
            walked, all_ids,
            "case page size {page_size}: expected every entry once, in order"
        );
    }

    // A token that names no entry of the list has no page.
    for token in ["synthetic", "nope", ""] {
        assert!(
            state::blueprint_page(&reported, 2, Some(token)).is_none(),
            "case pageToken {token:?}: expected no page"
        );
    }
}

// ---------------------------------------------------------------------------
// The catalog against the rest of the service
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_instance_can_be_created_from_an_active_reported_blueprint_only() {
    let emulator = Emulator::new();
    let (reported, _) = walk(&emulator, json!({"includeInactive": true})).await;

    // Rows: an operating system, a Windows image, an application stack — and a
    // retired entry, which "Use active blueprints when creating new instances"
    // refuses.
    for (index, case) in ["amazon_linux_2023", "windows_server_2019", "wordpress", "debian_10"]
        .iter()
        .enumerate()
    {
        let blueprint = reported
            .iter()
            .find(|blueprint| member(blueprint, "blueprintId") == *case)
            .unwrap_or_else(|| panic!("case {case:?}: expected the catalog to carry it"));
        let is_active = blueprint["isActive"] == json!(true);
        let name = format!("blueprint-row-{index}");
        let body = json!({
            "instanceNames": [name],
            "availabilityZone": "us-east-1a",
            "blueprintId": case,
            "bundleId": "micro_3_0",
        });

        if !is_active {
            let (status, refused) = emulator.call("CreateInstances", body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: an inactive blueprint cannot be created from, got {refused:?}"
            );
            // `CreateInstances` answers a blueprint that cannot be launched from
            // with Lightsail's `DoesNotExist` envelope — HTTP 400, wire error
            // `NotFoundException`, "does not exist or is deprecated" — which is
            // the refusal a retired entry earns.
            assert_eq!(
                error_code(&refused),
                "NotFoundException",
                "case {case:?}: expected the modelled NotFoundException, got {refused:?}"
            );
            continue;
        }

        emulator.ok("CreateInstances", body).await;
        let read = emulator
            .ok("GetInstance", json!({"instanceName": name}))
            .await;
        let instance = &read["instance"];
        assert_eq!(
            instance["blueprintId"].as_str(),
            Some(*case),
            "case {case:?}: expected the instance to name its blueprint, got {instance:?}"
        );
        // The catalog's `name` for a blueprint is the name an instance created
        // from it reports: one derivation, not two.
        assert_eq!(
            instance["blueprintName"].as_str(),
            blueprint["name"].as_str(),
            "case {case:?}: expected the catalog's own name, got {instance:?}"
        );
    }
}

#[tokio::test]
async fn reading_the_catalog_does_not_change_what_a_later_caller_sees() {
    let emulator = Emulator::new();
    let first = emulator.ok(OPERATION, json!({})).await;
    let second = emulator.ok(OPERATION, json!({})).await;
    let after_other_reads = {
        let _ = walk(&emulator, json!({"includeInactive": true})).await;
        let _ = emulator.call(OPERATION, json!({"appCategory": "LfR"})).await;
        let _ = emulator
            .call(OPERATION, json!({"pageToken": "not_a_blueprint"}))
            .await;
        emulator.ok(OPERATION, json!({})).await
    };

    assert_eq!(
        first, second,
        "case repeated read: the catalog must answer identically, got {first:?} then {second:?}"
    );
    assert_eq!(
        first, after_other_reads,
        "case read after other reads: the catalog must answer identically, got {first:?} \
         then {after_other_reads:?}"
    );
}
