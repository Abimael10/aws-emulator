// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the published base-image BATCHES the catalog read serves, and for
//! the projection each published image is reported with.
//!
//! Every case drives the real awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than over one
//! recorded request:
//!
//! * AWS refreshes its published base images in dated batches and keeps the earlier
//!   batches published, so the catalog a client reads carries MORE THAN ONE release batch
//!   and a read addressing an image of ANY of them — by `Names`, by `Arns` or by naming
//!   none at all — is answered with that image rather than refused;
//! * every batch publishes the same families, so a client that wants one platform can
//!   address it under every batch the catalog carries;
//! * an image reports the batch it belongs to consistently: the release date its name
//!   carries, the instant it reports as its release, the instant it was created and the
//!   release of the recipe it was built from are one and the same, and ordering the
//!   batches by the date in their names orders them by the instant they report;
//! * each published image is reported with the whole projection a client reads, including
//!   the members the on-disk model does not declare (`src/ops/types/image_wire.rs`);
//! * the catalog is STORED state and not a name the read synthesises: a name or an ARN of
//!   the published form whose batch the catalog does not carry is still refused, and a
//!   read that mixes an addressable image with an absent one is refused as a whole.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::images::{catalog, describe, published_image_arn, reported_names, walk};
use crate::refusal_messages::{not_found_sentence, refusal};
use crate::stack_fleet_lifecycle::Emulator;

/// The word AWS's refusal sentences use for an image.
const IMAGE_KIND: &str = "image";

/// The status this service serves a refused request over — the one status every refusal of
/// this service leaves with, whatever the refused shape's own `@httpError` trait states
/// (`crate::state::modelled_error_status` in the service crate).
const REFUSAL_STATUS: u16 = 400;

/// The wire code a refusal for a resource this account holds nothing under carries.
const NOT_FOUND_CODE: &str =
    service_com_amazonaws_appstream_photonadminproxyservice::state::RESOURCE_NOT_FOUND_CODE;

/// The release date a published image's name ends in, `(year, month, day)` read from the
/// `<MM-DD-YYYY>` tail AWS's base-image naming carries.
fn batch_of(name: &str) -> (String, String, String) {
    // `rsplitn` walks from the end, so the fields are the year, the day and the month.
    let tail: Vec<&str> = name.rsplitn(4, '-').collect();
    assert!(
        tail.len() == 4,
        "case {name:?}: a published name ends in its release date MM-DD-YYYY"
    );
    (tail[0].to_string(), tail[2].to_string(), tail[1].to_string())
}

/// The published catalog grouped by the release batch each image belongs to.
async fn batches(emulator: &Emulator) -> BTreeMap<(String, String, String), Vec<Value>> {
    let mut grouped: BTreeMap<(String, String, String), Vec<Value>> = BTreeMap::new();
    for image in catalog(emulator).await {
        let name = image["Name"]
            .as_str()
            .unwrap_or_else(|| panic!("every image states a Name, got {image}"))
            .to_string();
        grouped.entry(batch_of(&name)).or_default().push(image);
    }
    grouped
}

/// The `Name` member of one reported image.
fn name_of(image: &Value) -> String {
    image["Name"].as_str().unwrap_or("<no Name>").to_string()
}

/// One reported instant, in epoch seconds as the awsJson protocols carry a timestamp.
fn instant(image: &Value, member: &str) -> f64 {
    image[member].as_f64().unwrap_or_else(|| {
        panic!("case {:?}: {member} must be reported, got {image}", name_of(image))
    })
}

#[tokio::test]
async fn every_published_release_batch_is_addressable_by_name_and_by_arn() {
    let region = "us-west-2";
    let emulator = Emulator::in_region(region);
    let batches = batches(&emulator).await;

    assert!(
        batches.len() > 1,
        "case \"the published catalog\": AWS keeps earlier batches published, so the catalog \
         carries more than one release batch, got {:?}",
        batches.keys().collect::<Vec<_>>()
    );

    // Rows: every release batch the catalog publishes. Each is addressable in full by the
    // names of its images and by their ARNs, and reports exactly those images.
    for (batch, images) in &batches {
        let names: Vec<String> = images.iter().map(name_of).collect();
        let case = format!("the batch {batch:?} by name");
        let reported = reported_names(&walk(&emulator, &case, json!({ "Names": names })).await);
        let expected: Vec<String> = {
            let mut sorted = images.iter().map(name_of).collect::<Vec<String>>();
            sorted.sort();
            sorted
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );

        let arns: Vec<String> = images
            .iter()
            .map(|image| published_image_arn(region, &name_of(image)))
            .collect();
        let case = format!("the batch {batch:?} by ARN");
        let reported = reported_names(&walk(&emulator, &case, json!({ "Arns": arns })).await);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
    }

    // Every batch publishes the same set of families, so a client that has picked a
    // platform and an instance family can address it under every batch.
    let families_of = |images: &Vec<Value>| -> BTreeSet<String> {
        images
            .iter()
            .map(|image| {
                format!(
                    "{}|{}",
                    image["Platform"].as_str().unwrap_or_default(),
                    image["InstanceFamily"].as_str().unwrap_or_default()
                )
            })
            .collect()
    };
    let (first_batch, first_images) = batches
        .iter()
        .next()
        .expect("the catalog publishes at least one batch");
    let expected = families_of(first_images);
    for (batch, images) in &batches {
        let case = format!("the families of batch {batch:?}");
        let reported = families_of(images);
        assert_eq!(
            reported, expected,
            "case {case:?}: every batch publishes the families of {first_batch:?}, \
             got {reported:?}"
        );
    }
}

#[tokio::test]
async fn an_image_reports_the_batch_its_name_carries() {
    let emulator = Emulator::in_region("eu-west-1");
    let batches = batches(&emulator).await;

    // Rows: every batch. Every image of one batch reports the same release instant, every
    // member that states that release agrees with it, and the instant is midnight of the
    // day the names carry — the batch is released on a day, so the instant is that day.
    let mut released: BTreeMap<(String, String, String), f64> = BTreeMap::new();
    for (batch, images) in &batches {
        for image in images {
            let case = format!("{batch:?} {}", name_of(image));
            let release = instant(image, "PublicBaseImageReleasedDate");
            assert_eq!(
                instant(image, "CreatedTime"),
                release,
                "case {case:?}: a published image was created when AWS released it, got {image}"
            );
            assert_eq!(
                instant(image, "appStreamRecipeReleaseTimestamp"),
                release,
                "case {case:?}: the recipe is released with the image built from it, got {image}"
            );
            assert_eq!(
                release % 86_400.0,
                0.0,
                "case {case:?}: a batch is released on a day, so its instant is that day's \
                 midnight, got {release}"
            );
            let known = released.entry(batch.clone()).or_insert(release);
            assert_eq!(
                *known, release,
                "case {case:?}: every image of one batch reports that batch's release, \
                 got {release} and {known}"
            );
        }
    }

    // Ordering the batches by the date their names carry orders them by the instant they
    // report: the reported release is derived from the batch rather than chosen per image.
    let by_name: Vec<f64> = released.values().copied().collect();
    let mut by_instant = by_name.clone();
    by_instant.sort_by(f64::total_cmp);
    assert_eq!(
        by_name, by_instant,
        "case \"the published batches\": the date in a name orders the batches the way the \
         instants they report do, got {by_name:?}"
    );
}

#[tokio::test]
async fn a_published_image_is_reported_with_the_whole_projection_a_client_reads() {
    // Rows: the Region the request is signed for — every member below is either the
    // image's own or derived from the Region the request addresses.
    for region in ["us-west-2", "ap-southeast-2"] {
        for image in catalog(&Emulator::in_region(region)).await {
            let name = name_of(&image);
            let case = format!("{region} {name}");

            // The family the image reports as its own, as a token: the hardware it runs
            // on, in upper case with its words joined by underscores. An image that states
            // ONE family runs on that family's hardware, so the token is that family's; an
            // image that states several (one built with a GPU vendor's whole driver) runs
            // on the vendor's own hardware, so the token is `GRAPHICS_<the accelerator its
            // name carries>`.
            let families: Vec<String> = image["SupportedInstanceFamilies"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|family| family.as_str().unwrap_or_default().to_string())
                .collect();
            let accelerator = name
                .strip_prefix("AppStream-Graphics-")
                .and_then(|tail| tail.split('-').next())
                .map(str::to_uppercase);
            let expected = match accelerator {
                // An accelerated image that states ONE family runs on that family's
                // hardware; one that states several runs on the vendor's own.
                Some(accelerator) if families.len() > 1 => {
                    Some(format!("GRAPHICS_{accelerator}"))
                }
                // A general-purpose image reports the first of the families it serves.
                _ => families
                    .first()
                    .map(|family| family.to_uppercase().replace(' ', "_")),
            };
            assert_eq!(
                image["InstanceFamily"].as_str().map(str::to_string),
                expected,
                "case {case:?}: expected InstanceFamily {expected:?}, got {image}"
            );

            // The members a published base image reports with one value each: AWS owns the
            // licence and the software, nothing is shared with another account, the agent
            // is the one of the image's own batch, and the applications are the image's
            // own rather than a catalog resolved at session start.
            let rows: Vec<(&str, Value)> = vec![
                ("Byol", json!(false)),
                ("ManagedSoftwareIncluded", json!(false)),
                ("DynamicApplicationCatalogEnabled", json!(false)),
                ("DynamicAppProvidersEnabled", json!("DISABLED")),
                ("ImageSharedWithOthers", json!("FALSE")),
                ("LatestAppstreamAgentVersion", json!("FALSE")),
                ("Applications", json!([])),
                ("ImageErrors", json!([])),
                (
                    // The model's `VolumeConfig$VolumeSizeInGb` doc: "The default is 200
                    // GB, which is included in the hourly instance rate".
                    "RootVolumeConfig",
                    json!({ "VolumeSizeInGb": 200, "VolumeType": "gp2" }),
                ),
                // The provenance members an image reports — the recipe it was built from,
                // the instance type groups it serves and its software metadata — are the
                // published catalog's own, and are asserted over the whole catalog in
                // `tests/all/published_image_provenance.rs`.
            ];
            for (member, expected) in rows {
                assert_eq!(
                    image[member], expected,
                    "case {case:?}: expected {member} {expected}, got {image}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_published_name_of_a_batch_the_catalog_does_not_carry_is_refused() {
    let region = "us-west-2";
    let emulator = Emulator::in_region(region);
    let stored = reported_names(&catalog(&emulator).await);
    let one = stored[0].clone();
    let (year, month, day) = batch_of(&one);
    let head = one
        .strip_suffix(&format!("-{month}-{day}-{year}"))
        .unwrap_or_else(|| panic!("case {one:?}: a published name ends in its release date"))
        .to_string();

    // Rows: names of exactly the published form whose batch the catalog does not carry —
    // a day before any batch, a day after every batch, and a day no batch was released
    // on. The read is backed by stored images, so a well-formed name is not an image.
    let absent: Vec<String> = ["01-01-1999", "12-31-2099", "02-29-2024"]
        .iter()
        .map(|date| format!("{head}-{date}"))
        .filter(|name| !stored.contains(name))
        .collect();
    assert_eq!(
        absent.len(),
        3,
        "case \"the absent batches\": none of these dates may be a batch the catalog \
         carries, got {absent:?}"
    );

    for name in &absent {
        let case = format!("Names [{name:?}]");
        let message = refusal(
            &emulator,
            &case,
            "DescribeImages",
            json!({ "Names": [name] }),
            REFUSAL_STATUS,
            NOT_FOUND_CODE,
        )
        .await;
        let expected = not_found_sentence(IMAGE_KIND, name);
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );

        let case = format!("Arns [{name:?}]");
        let message = refusal(
            &emulator,
            &case,
            "DescribeImages",
            json!({ "Arns": [published_image_arn(region, name)] }),
            REFUSAL_STATUS,
            NOT_FOUND_CODE,
        )
        .await;
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );
    }

    // Rows: a read that names one image the catalog carries and one it does not, in either
    // order. A read addresses the images it asks about, so it is refused as a whole and
    // answers with none of them.
    for names in [
        vec![one.clone(), absent[0].clone()],
        vec![absent[0].clone(), one.clone()],
    ] {
        let case = format!("Names {names:?}");
        let message = refusal(
            &emulator,
            &case,
            "DescribeImages",
            json!({ "Names": names }),
            REFUSAL_STATUS,
            NOT_FOUND_CODE,
        )
        .await;
        let expected = not_found_sentence(IMAGE_KIND, &absent[0]);
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );
    }

    // The refusals above are this read's not-found path and not a blanket refusal: on the
    // same emulator the image that IS published is still answered.
    let (images, _) = describe(
        &emulator,
        "the stored image, after the refusals",
        json!({ "Names": [one.clone()] }),
    )
    .await;
    assert_eq!(
        reported_names(&images),
        vec![one.clone()],
        "case \"the stored image, after the refusals\": expected {one:?}, got {images:?}"
    );
}
