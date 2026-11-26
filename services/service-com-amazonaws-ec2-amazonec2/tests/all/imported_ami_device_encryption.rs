// SPDX-License-Identifier: CC0-1.0

//! The members an AMI a VM import created reports about its Amazon EBS devices, end to end
//! over the wire the service speaks — `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! THE RULE: every Amazon EBS device of every image a read reports carries the modelled
//! `encrypted` member, and its value is the state that device restores with. For an AMI whose
//! block device mapping the service DERIVES rather than a request supplying it, that state is
//! the one the image's creation asked for — "specifies whether the destination AMI of the
//! imported image should be encrypted" (`ImportImageRequest$Encrypted`) — so the AMI of an
//! encrypted import reports `true`, and the AMI of an import that asked for no encryption
//! reports `false` rather than reporting nothing at all. On the same rows an image's
//! `freeTierEligible` follows its own billing: nothing bills an AMI an import of this account
//! created, so each of them is eligible.
//!
//! Every row of the table is a distinct input along what the rule generalises over: whether
//! the import named an encryption state and which, whether it named a KMS key, which platform
//! it imported, how many disks it converted and in which format. The fact is then read through
//! BOTH readers that report it — the image's own mapping and the snapshot that mapping restores
//! from, which cannot disagree about one device — and over the account's own-images page at two
//! page sizes, which is the shape a client that walks its AMI inventory sees.

use std::collections::BTreeMap;

use ec2::state::IMPORT_TASK_STATE_COMPLETED;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, member};
use super::rbin_retention_rules::poll_until;
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// How many pages the own-images walk reads before it gives up: far above the catalog this
/// suite reads at its smallest page size, so a `NextToken` that never clears fails the case
/// instead of looping for ever.
const MAX_PAGES: usize = 500;

/// One call that must answer HTTP 200, as its body.
async fn ok(router: &MultiServiceRouter, case: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One Amazon EBS device of an image, as a read reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Device {
    name: String,
    snapshot_id: Option<String>,
    /// The `encrypted` member of the device's Amazon EBS half — `None` where the response
    /// carried no such member at all, which is the case a client cannot tell from
    /// "unencrypted".
    encrypted: Option<String>,
}

/// One image as a read reports it: the members this rule is about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedImage {
    image_id: String,
    devices: Vec<Device>,
    free_tier_eligible: Option<String>,
}

/// Every image of one `DescribeImages` body: the element that carries an `imageId`.
fn reported_images(case: &str, body: &str) -> Vec<ReportedImage> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "imageId").is_some())
        .map(|image| ReportedImage {
            image_id: child_text(image, "imageId").unwrap_or_default(),
            free_tier_eligible: child_text(image, "freeTierEligible"),
            devices: set_items(image, "blockDeviceMapping")
                .into_iter()
                .filter_map(|mapping| {
                    let ebs = child(mapping, "ebs")?;
                    Some(Device {
                        name: child_text(mapping, "deviceName").unwrap_or_default(),
                        snapshot_id: child_text(ebs, "snapshotId"),
                        encrypted: child_text(ebs, "encrypted"),
                    })
                })
                .collect(),
        })
        .collect()
}

/// One row of the import table: the parameters beside the disk containers, the containers
/// themselves, and the encryption state every Amazon EBS device of the AMI must then report.
struct Row {
    case: &'static str,
    params: Vec<Param>,
    /// What `encrypted` must be reported as, as the wire spells a boolean.
    encrypted: &'static str,
}

/// One disk container naming its image by an S3 URL in `format`.
fn container(index: usize, format: &str, url: &str) -> Vec<Param> {
    vec![
        param(&format!("DiskContainer.{index}.Format"), format),
        param(&format!("DiskContainer.{index}.Url"), url),
    ]
}

/// The imports this suite drives the rule over.
///
/// The formats are values of the published catalog (`ImageDiskContainer$Format`, as
/// `IMPORT_DISK_IMAGE_FORMATS` carries it) and the platform is one of the two the import
/// documents, so no row is a value invented here.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an import that names no encryption at all",
            params: container(1, "VMDK", "s3://imports/plain-0.vmdk"),
            encrypted: "false",
        },
        Row {
            case: "an import that asks for an unencrypted destination",
            params: [
                container(1, "RAW", "s3://imports/plain-1.raw"),
                vec![param("Encrypted", "false")],
            ]
            .concat(),
            encrypted: "false",
        },
        Row {
            case: "an import that asks for an encrypted destination",
            params: [
                container(1, "VHD", "s3://imports/secret-0.vhd"),
                vec![param("Encrypted", "true")],
            ]
            .concat(),
            encrypted: "true",
        },
        Row {
            case: "an encrypted import under a named KMS key",
            params: [
                container(1, "VHDX", "s3://imports/secret-1.vhdx"),
                vec![
                    param("Encrypted", "true"),
                    param("KmsKeyId", "alias/aws/ebs"),
                ],
            ]
            .concat(),
            encrypted: "true",
        },
        Row {
            case: "an encrypted Windows import of two disks",
            params: [
                container(1, "OVA", "s3://imports/secret-win-0.ova"),
                container(2, "VMDK", "s3://imports/secret-win-1.vmdk"),
                vec![
                    param("Platform", "Windows"),
                    param("Encrypted", "true"),
                    param("Architecture", "x86_64"),
                ],
            ]
            .concat(),
            encrypted: "true",
        },
        Row {
            case: "an unencrypted Windows import of two disks",
            params: [
                container(1, "VMDK", "s3://imports/plain-win-0.vmdk"),
                container(2, "RAW", "s3://imports/plain-win-1.raw"),
                vec![
                    param("Platform", "Windows"),
                    param("Architecture", "x86_64"),
                ],
            ]
            .concat(),
            encrypted: "false",
        },
    ]
}

/// The one import task of a `DescribeImportImageTasks` or `ImportImage` body, as
/// `(status, imageId)`.
fn task_state(case: &str, body: &str) -> (Option<String>, Option<String>) {
    let parsed = document(case, body);
    let task = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "importTaskId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no import task in {body}"));
    (child_text(task, "status"), child_text(task, "imageId"))
}

/// Start the import `row` describes and answer the task id it created.
async fn start_import(router: &MultiServiceRouter, row: &Row) -> String {
    let body = ok(router, row.case, "ImportImage", &row.params).await;
    element(&body, "importTaskId")
        .unwrap_or_else(|| panic!("case {:?}: ImportImage started no task: {body}", row.case))
}

/// The AMI the import `task_id` created, once the clock has completed the task.
///
/// The import is `active` when it is started and the CLOCK completes it, so the task is looked
/// at AGAIN in a loop until the describe reports it `completed` or a generous deadline passes:
/// nothing here waits for a span or depends on how fast the host runs.
async fn imported_image_id(router: &MultiServiceRouter, case: &str, task_id: &str) -> String {
    let narrowed = vec![param("ImportTaskId.1", task_id)];
    poll_until(case, "the import to complete with its AMI", || async {
        let body = ok(router, case, "DescribeImportImageTasks", &narrowed).await;
        let (status, image_id) = task_state(case, &body);
        status.as_deref() == Some(IMPORT_TASK_STATE_COMPLETED) && image_id.is_some()
    })
    .await;
    let body = ok(router, case, "DescribeImportImageTasks", &narrowed).await;
    task_state(case, &body)
        .1
        .unwrap_or_else(|| panic!("case {case:?}: the completed import reports no AMI: {body}"))
}

/// Every row's import, started together and then waited on, as `(row, the AMI it created)`.
///
/// Started together because each import's completion is owed by the clock: the whole table
/// then settles in about one transition span rather than one per row.
async fn imported_table(router: &MultiServiceRouter) -> Vec<(Row, String)> {
    let rows = rows();
    let mut tasks: Vec<String> = Vec::with_capacity(rows.len());
    for row in &rows {
        tasks.push(start_import(router, row).await);
    }
    let mut imported: Vec<(Row, String)> = Vec::with_capacity(rows.len());
    for (row, task_id) in rows.into_iter().zip(tasks) {
        let image_id = imported_image_id(router, row.case, &task_id).await;
        imported.push((row, image_id));
    }
    imported
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
        params.push(param("MaxResults", &page_size.to_string()));
        if let Some(cursor) = token.as_deref() {
            params.push(param("NextToken", cursor));
        }
        let body = ok(router, case, "DescribeImages", &params).await;
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

/// Assert the rule over one image a read reported: every Amazon EBS device reports the
/// encryption state `encrypted`, and the image is eligible for the Free Tier.
fn assert_image(case: &str, image: &ReportedImage, encrypted: &str) {
    assert!(
        !image.devices.is_empty(),
        "case {case:?}: an EBS-backed AMI maps at least one Amazon EBS device, got {image:?}"
    );
    for device in &image.devices {
        assert_eq!(
            device.encrypted.as_deref(),
            Some(encrypted),
            "case {case:?}: device {:?} of {} must report encrypted={encrypted:?}, got {:?}",
            device.name,
            image.image_id,
            device.encrypted
        );
    }
    assert_eq!(
        image.free_tier_eligible.as_deref(),
        Some("true"),
        "case {case:?}: nothing bills {}, so it must report freeTierEligible \"true\", got {:?}",
        image.image_id,
        image.free_tier_eligible
    );
}

/// THE RULE on the image the import created, and on the snapshot its device restores from:
/// the AMI reports the encryption state the import asked for, and the two readers of that one
/// device agree about it.
#[tokio::test]
async fn an_imported_ami_reports_the_encryption_state_its_import_asked_for() {
    let (router, _engine) = super::ec2_query::emulator();
    let imported = imported_table(&router).await;

    for (row, image_id) in &imported {
        let case = row.case;
        let body = ok(&router, case, "DescribeImages", &[param("ImageId.1", image_id)]).await;
        let images = reported_images(case, &body);
        assert_eq!(
            images.len(),
            1,
            "case {case:?}: expected exactly one image for {image_id}, got {images:?}"
        );
        let image = &images[0];
        assert_image(case, image, row.encrypted);

        for device in &image.devices {
            let snapshot_id = device.snapshot_id.clone().unwrap_or_else(|| {
                panic!(
                    "case {case:?}: device {:?} of {image_id} restores from a snapshot, so it \
                     must report its id, got {device:?}",
                    device.name
                )
            });
            let snapshot = ok(
                &router,
                case,
                "DescribeSnapshots",
                &[param("SnapshotId.1", &snapshot_id)],
            )
            .await;
            assert_eq!(
                element(&snapshot, "encrypted").as_deref(),
                Some(row.encrypted),
                "case {case:?}: {snapshot_id} is what device {:?} restores, so it must report \
                 encrypted={:?}, got {snapshot}",
                device.name,
                row.encrypted
            );
        }
    }

    // Neither value is the constant the assertions above pass against.
    for want in ["true", "false"] {
        assert!(
            imported.iter().any(|(row, _)| row.encrypted == want),
            "the table must carry an import whose AMI reports encrypted {want:?}"
        );
    }
}

/// THE RULE over the account's OWN IMAGES page, which is the read a client walking its AMI
/// inventory makes: every row of every page reports the encryption state of each of its Amazon
/// EBS devices and the Free Tier eligibility its own billing gives it, however the read is cut
/// into pages.
#[tokio::test]
async fn every_row_of_the_own_images_page_reports_device_encryption_and_free_tier() {
    let (router, _engine) = super::ec2_query::emulator();
    let imported = imported_table(&router).await;
    let expected: BTreeMap<String, (&'static str, &'static str)> = imported
        .iter()
        .map(|(row, image_id)| (image_id.clone(), (row.case, row.encrypted)))
        .collect();

    for page_size in [1usize, 1_000] {
        let case = format!("the account's own images at MaxResults={page_size}");
        let reported = every_page(&router, &case, &[param("Owner.1", "self")], page_size).await;

        for (image_id, (row, encrypted)) in &expected {
            let image = reported
                .iter()
                .find(|image| &image.image_id == image_id)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the account owns {image_id} ({row:?}), so the read must \
                         report it, got {reported:?}"
                    )
                });
            assert_image(&format!("{case}: {row}"), image, encrypted);
        }
    }
}
