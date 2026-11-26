// SPDX-License-Identifier: CC0-1.0

//! The members an AMI read carries on EVERY item of EVERY page, end to end over the wire the
//! service speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! Two invariants of the IMAGE RECORD, asserted over whole result sets rather than over one
//! image fetched by id:
//!
//! 1. EVERY AMAZON EBS DEVICE OF EVERY REPORTED IMAGE REPORTS THE ENCRYPTION STATE IT
//!    RESTORES WITH. The state is the device's own — the snapshot's state for a device that
//!    restores one, and unencrypted for a new empty volume — and the member is REPORTED with
//!    the value `false` rather than left out, so a client reading an image's encryption
//!    posture can tell "unencrypted" from "not reported".
//! 2. AN IMAGE'S FREE TIER ELIGIBILITY FOLLOWS ITS OWN BILLING, on every item of the same
//!    read: an AMI billed through a product code of its own charges for its software beside
//!    the instance hours the Free Tier covers and so cannot be launched under those limits,
//!    while an image carrying no such code can, whoever published it.
//!
//! [`super::read_optional_members`] pins both rules for ONE image resolved by its id. What
//! this suite adds is the PAGE: the rules are facts of each record, so they hold for every
//! item of every page a read cuts the catalog into, whichever writer stored the image and
//! however many images the page carries — the shape a client that walks the catalog sees.
//! Every row is a distinct input along what that generalises over: the page size, how many
//! devices the AMI maps, whether each device restores an encrypted snapshot, an unencrypted
//! one or no snapshot at all, which writer stored the image, and whether the image is billed
//! through a product code.

use std::collections::BTreeMap;

use ec2::ops::types::ProductCode;
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec, image_is_free_tier_eligible,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, member};
use super::snapshots_in_recycle_bin::{SnapshotRow, create_snapshot};
use super::volume_lifecycle::{REGION, p};
use super::wire::{Param, child, child_text, document, set_items};

/// How many pages a walk below reads before it gives up: far above the catalog this suite
/// reads at its smallest page size, so a `NextToken` that never clears fails the case
/// instead of looping for ever.
const MAX_PAGES: usize = 500;

/// One `action` call signed for the Region the volume and snapshot suites work in, so the
/// snapshots this suite mints through their shared helper and the registers that restore
/// from them are made in one Region.
async fn op(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call that must answer HTTP 200, as its body.
async fn served(router: &MultiServiceRouter, case: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = op(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One image as a read reports it: the members these invariants are about, with every
/// optional one as `Some` only where the response carried it at all.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedImage {
    image_id: String,
    name: String,
    /// `(device name, the `encrypted` member of its Amazon EBS half)` per Amazon EBS device,
    /// in the order the image reported them — `None` for a device whose Amazon EBS half
    /// reports no `encrypted` member at all.
    devices: Vec<(String, Option<String>)>,
    free_tier_eligible: Option<String>,
    /// `(product code, type)` per entry of the image's `productCodes`.
    product_codes: Vec<(String, String)>,
}

impl ReportedImage {
    /// The product codes the image reports, as the modelled shape the crate's one reading of
    /// Free Tier eligibility takes.
    fn modelled_product_codes(&self) -> Vec<ProductCode> {
        self.product_codes
            .iter()
            .map(|(code, code_type)| ProductCode {
                product_code_id: Some(code.clone()),
                product_code_type: Some(code_type.clone()),
            })
            .collect()
    }
}

/// Every image of one response body: the element that carries an `imageId`.
fn reported_images(case: &str, body: &str) -> Vec<ReportedImage> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "imageId").is_some())
        .map(|image| ReportedImage {
            image_id: child_text(image, "imageId").unwrap_or_default(),
            name: child_text(image, "name").unwrap_or_default(),
            devices: set_items(image, "blockDeviceMapping")
                .into_iter()
                .filter_map(|mapping| {
                    let device_name = child_text(mapping, "deviceName").unwrap_or_default();
                    let ebs = child(mapping, "ebs")?;
                    Some((device_name, child_text(ebs, "encrypted")))
                })
                .collect(),
            free_tier_eligible: child_text(image, "freeTierEligible"),
            product_codes: set_items(image, "productCodes")
                .into_iter()
                .map(|code| {
                    (
                        child_text(code, "productCode").unwrap_or_default(),
                        child_text(code, "type").unwrap_or_default(),
                    )
                })
                .collect(),
        })
        .collect()
}

/// Every image a `DescribeImages` narrowed by `scope` reports, walked one `page_size`-sized
/// page at a time through the `NextToken` the previous page handed back.
async fn every_page(
    router: &MultiServiceRouter,
    case: &str,
    scope: &[Param],
    page_size: usize,
) -> Vec<ReportedImage> {
    let mut collected: Vec<ReportedImage> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut params = scope.to_vec();
        params.push(p("MaxResults", page_size.to_string()));
        if let Some(cursor) = token.as_deref() {
            params.push(p("NextToken", cursor));
        }
        let body = served(router, case, "DescribeImages", &params).await;
        let page = reported_images(case, &body);
        assert!(
            page.len() <= page_size,
            "case {case:?}: a page carries at most the {page_size} images asked for, got {}",
            page.len()
        );
        collected.extend(page);
        token = member(&body, "nextToken").filter(|cursor| !cursor.is_empty());
        if token.is_none() {
            return collected;
        }
    }
    panic!("case {case:?}: DescribeImages still handed back a nextToken after {MAX_PAGES} pages")
}

/// Mint one snapshot of this account of `size` GiB, encrypted or not, through the operations
/// that create one, and answer its id.
async fn snapshot(
    router: &MultiServiceRouter,
    case: &'static str,
    size: i32,
    encrypted: bool,
) -> String {
    let (snapshot_id, _volume_id) = create_snapshot(
        router,
        &SnapshotRow {
            case,
            volume_size: size,
            encrypted,
            description: None,
            tags: &[],
        },
    )
    .await;
    snapshot_id
}

/// One product code of a paid image.
fn product_code(id: &str, code_type: &str) -> ProductCode {
    ProductCode {
        product_code_id: Some(id.to_string()),
        product_code_type: Some(code_type.to_string()),
    }
}

/// Store one AMI of this account the way a register would, deriving its root block device
/// mapping, and answer its id.
///
/// The path a PAID image reaches this store by: no operation of this emulator is authorized
/// to claim billing product codes, so a Marketplace appliance is seeded rather than
/// registered.
fn seeded_image(engine: &engine::InMemoryEngine, name: &str, codes: Vec<ProductCode>) -> String {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    guard.insert_image(ImageSpec {
        name: name.to_string(),
        product_codes: codes,
        ..ImageSpec::default()
    })
}

/// One Amazon EBS device of a register: the device it names, and the one `Ebs` member that
/// describes what it restores.
struct Device {
    device_name: &'static str,
    /// `Ebs.SnapshotId` for a device restored from a snapshot, `Ebs.VolumeSize` for a new
    /// empty volume.
    ebs_member: &'static str,
    ebs_value: String,
    /// What `encrypted` must then be reported as for this device.
    encrypted: &'static str,
}

/// One row of the register table.
struct RegisterRow {
    case: &'static str,
    root_device: &'static str,
    devices: Vec<Device>,
}

/// The form fields of one register's block device mappings.
fn mapping_fields(devices: &[Device]) -> Vec<Param> {
    devices
        .iter()
        .enumerate()
        .flat_map(|(index, device)| {
            let at = index + 1;
            vec![
                p(
                    &format!("BlockDeviceMapping.{at}.DeviceName"),
                    device.device_name,
                ),
                p(
                    &format!("BlockDeviceMapping.{at}.{}", device.ebs_member),
                    device.ebs_value.clone(),
                ),
            ]
        })
        .collect()
}

/// What one image of the catalog must report: the case that put it there, the
/// `(device name, encrypted)` pairs of its Amazon EBS devices and its Free Tier eligibility.
struct Expected {
    case: String,
    devices: Vec<(String, String)>,
    free_tier_eligible: &'static str,
}

/// INVARIANT 1 and 2 over whole result sets: every item of every page reports the encryption
/// state of each of its Amazon EBS devices and the Free Tier eligibility its own billing
/// gives it.
///
/// The catalog read is the account's own images, written by both writers that store one — a
/// register over the wire, with its mappings, and the store's own seeding, whose root mapping
/// is derived — and the rows differ along how many devices the AMI maps, which of them restore
/// an encrypted snapshot, an unencrypted one or no snapshot at all, and whether the image is
/// billed through a product code. The whole set is then read at five page sizes, so no row and
/// no page shape is the rule.
#[tokio::test]
async fn every_item_of_every_page_reports_device_encryption_and_free_tier_eligibility() {
    let (router, engine) = super::ec2_query::emulator();

    let plain = snapshot(&router, "the table's unencrypted snapshot", 8, false).await;
    let secret = snapshot(&router, "the table's encrypted snapshot", 20, true).await;

    let rows: [RegisterRow; 5] = [
        RegisterRow {
            case: "one root device restored from an unencrypted snapshot",
            root_device: "/dev/xvda",
            devices: vec![Device {
                device_name: "/dev/xvda",
                ebs_member: "Ebs.SnapshotId",
                ebs_value: plain.clone(),
                encrypted: "false",
            }],
        },
        RegisterRow {
            case: "one root device restored from an encrypted snapshot",
            root_device: "/dev/xvda",
            devices: vec![Device {
                device_name: "/dev/xvda",
                ebs_member: "Ebs.SnapshotId",
                ebs_value: secret.clone(),
                encrypted: "true",
            }],
        },
        RegisterRow {
            case: "an unencrypted root beside an encrypted and an empty data device",
            root_device: "/dev/sda1",
            devices: vec![
                Device {
                    device_name: "/dev/sda1",
                    ebs_member: "Ebs.SnapshotId",
                    ebs_value: plain.clone(),
                    encrypted: "false",
                },
                Device {
                    device_name: "/dev/sdb",
                    ebs_member: "Ebs.SnapshotId",
                    ebs_value: secret.clone(),
                    encrypted: "true",
                },
                Device {
                    device_name: "/dev/sdc",
                    ebs_member: "Ebs.VolumeSize",
                    ebs_value: "4".to_string(),
                    encrypted: "false",
                },
            ],
        },
        RegisterRow {
            case: "an encrypted root beside an unencrypted data device",
            root_device: "/dev/sda1",
            devices: vec![
                Device {
                    device_name: "/dev/sda1",
                    ebs_member: "Ebs.SnapshotId",
                    ebs_value: secret.clone(),
                    encrypted: "true",
                },
                Device {
                    device_name: "/dev/sdf",
                    ebs_member: "Ebs.SnapshotId",
                    ebs_value: plain.clone(),
                    encrypted: "false",
                },
            ],
        },
        RegisterRow {
            case: "a root device of a new empty volume, sized by the request",
            root_device: "/dev/xvda",
            devices: vec![Device {
                device_name: "/dev/xvda",
                ebs_member: "Ebs.VolumeSize",
                ebs_value: "12".to_string(),
                encrypted: "false",
            }],
        },
    ];

    let mut expected: BTreeMap<String, Expected> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let mut params = vec![
            p("Name", format!("catalog-member-row-{index}")),
            p("RootDeviceName", row.root_device),
        ];
        params.extend(mapping_fields(&row.devices));
        let body = served(&router, case, "RegisterImage", &params).await;
        let image_id = element(&body, "imageId")
            .unwrap_or_else(|| panic!("case {case:?}: RegisterImage minted no AMI: {body}"));
        expected.insert(
            image_id,
            Expected {
                case: case.to_string(),
                devices: row
                    .devices
                    .iter()
                    .map(|device| {
                        (
                            device.device_name.to_string(),
                            device.encrypted.to_string(),
                        )
                    })
                    .collect(),
                // Nothing bills an AMI a register of this account minted.
                free_tier_eligible: "true",
            },
        );
    }

    // The other writer of an image, whose root mapping the store derives rather than a
    // request supplying it: one image nobody sells, and two billed through codes of their own.
    let derived_root = ImageSpec::default().root_device_name;
    let seeded: Vec<(&str, Vec<ProductCode>, &'static str)> = vec![
        ("an image the store seeded, which nobody sells", Vec::new(), "true"),
        (
            "an image billed through one Marketplace code",
            vec![product_code("abc123prodcode", "marketplace")],
            "false",
        ),
        (
            "an image billed through a DevPay code and a Marketplace one",
            vec![
                product_code("aaa111bbb222ccc3", "devpay"),
                product_code("def456prodcode", "marketplace"),
            ],
            "false",
        ),
    ];
    for (index, (case, codes, free_tier_eligible)) in seeded.into_iter().enumerate() {
        let image_id = seeded_image(&engine, &format!("catalog-member-seeded-{index}"), codes);
        expected.insert(
            image_id,
            Expected {
                case: case.to_string(),
                devices: vec![(derived_root.clone(), "false".to_string())],
                free_tier_eligible,
            },
        );
    }

    // Every page size from one image per page to one page for the lot: the invariant is a
    // fact of each record, so how the read is cut up cannot change it.
    for page_size in [1usize, 2, 3, 5, 1_000] {
        let case = format!("the account's own images at MaxResults={page_size}");
        let reported = every_page(&router, &case, &[p("Owner.1", "self")], page_size).await;

        let reported_ids: Vec<String> = reported
            .iter()
            .map(|image| image.image_id.clone())
            .collect();
        let mut sorted_ids = reported_ids.clone();
        sorted_ids.sort();
        sorted_ids.dedup();
        let want_ids: Vec<String> = expected.keys().cloned().collect();
        assert_eq!(
            sorted_ids, want_ids,
            "case {case:?}: expected every seeded image once, got {reported_ids:?}"
        );

        for image in &reported {
            let want = expected
                .get(&image.image_id)
                .unwrap_or_else(|| panic!("case {case:?}: {} is not a seeded image", image.image_id));
            let row = &want.case;
            let devices: Vec<(String, Option<String>)> = want
                .devices
                .iter()
                .map(|(device, encrypted)| (device.clone(), Some(encrypted.clone())))
                .collect();
            assert_eq!(
                image.devices, devices,
                "case {case:?}: image {row:?} must report {devices:?} for its Amazon EBS \
                 devices, got {:?}",
                image.devices
            );
            assert_eq!(
                image.free_tier_eligible.as_deref(),
                Some(want.free_tier_eligible),
                "case {case:?}: image {row:?} must report freeTierEligible {:?}, got {:?}",
                want.free_tier_eligible,
                image.free_tier_eligible
            );
        }
    }
}

/// INVARIANT 2 across the WHOLE catalog a fresh account can see, with no owner term to narrow
/// it: every image a read reports carries `freeTierEligible`, and its value is the one the
/// image's own product codes give it — the crate's one reading of that question
/// (`image_is_free_tier_eligible`) over the codes the SAME response reported, rather than a
/// value written out per image.
///
/// Invariant 1 rides along on the same pages: every Amazon EBS device of every image reports
/// its encryption state. Both polarities are asserted to occur, so neither assertion can pass
/// vacuously, and the read is walked at two page sizes.
#[tokio::test]
async fn the_free_tier_flag_of_every_reported_image_follows_its_own_product_codes() {
    let (router, engine) = super::ec2_query::emulator();

    // A paid appliance of this account beside the catalog a fresh account already sees, so
    // the read carries images of both billing kinds.
    seeded_image(
        &engine,
        "catalog-paid-appliance",
        vec![product_code("abc123prodcode", "marketplace")],
    );
    let secret = snapshot(&router, "the suite's encrypted snapshot", 16, true).await;
    let case = "a register restoring an encrypted snapshot";
    served(
        &router,
        case,
        "RegisterImage",
        &[
            p("Name", "catalog-encrypted-root"),
            p("RootDeviceName", "/dev/xvda"),
            p("BlockDeviceMapping.1.DeviceName", "/dev/xvda"),
            p("BlockDeviceMapping.1.Ebs.SnapshotId", &secret),
        ],
    )
    .await;

    for page_size in [7usize, 1_000] {
        let case = format!("every image this account can see at MaxResults={page_size}");
        let scope = vec![
            p("IncludeDeprecated", "true"),
            p("IncludeDisabled", "true"),
        ];
        let reported = every_page(&router, &case, &scope, page_size).await;
        assert!(
            reported.len() > 2,
            "case {case:?}: a fresh account sees the published catalog beside its own images, \
             got {} images",
            reported.len()
        );

        for image in &reported {
            let name = &image.name;
            let eligible = image_is_free_tier_eligible(&image.modelled_product_codes());
            let want = if eligible { "true" } else { "false" };
            assert_eq!(
                image.free_tier_eligible.as_deref(),
                Some(want),
                "case {case:?}: image {name:?} is billed through {:?}, so it must report \
                 freeTierEligible {want:?}, got {:?}",
                image.product_codes,
                image.free_tier_eligible
            );
            for (device, encrypted) in &image.devices {
                assert!(
                    matches!(encrypted.as_deref(), Some("true") | Some("false")),
                    "case {case:?}: image {name:?} device {device:?} must report the \
                     encryption state it restores with, got {encrypted:?}"
                );
            }
        }

        for want in ["true", "false"] {
            assert!(
                reported
                    .iter()
                    .any(|image| image.free_tier_eligible.as_deref() == Some(want)),
                "case {case:?}: the read carries an image reporting freeTierEligible {want:?}, \
                 so neither value is the constant this asserts against"
            );
        }
        for want in ["true", "false"] {
            assert!(
                reported.iter().any(|image| image
                    .devices
                    .iter()
                    .any(|(_, encrypted)| encrypted.as_deref() == Some(want))),
                "case {case:?}: the read carries a device reporting encrypted {want:?}, so the \
                 member is the device's own state rather than one value for every image"
            );
        }
    }
}
