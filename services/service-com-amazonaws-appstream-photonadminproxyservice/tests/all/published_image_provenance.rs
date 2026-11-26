// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what the published base-image catalog PUBLISHES about each image:
//! the description AWS words for it, the build recipe it came out of, the instance type
//! groups it serves, the GPU driver its batch installed, and the accelerated images every
//! accelerated operating system is published with.
//!
//! Every case drives the real awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than over one
//! recorded request:
//!
//! * a published base image's description is the sentence AWS publishes for its operating
//!   system and its accelerator — words carried per operating system, which cannot be
//!   rendered from the platform token (two Linux releases are worded differently, and one
//!   operating system words its general-purpose and its accelerated image differently);
//! * `SupportedInstanceTypeGroups` is a member every image states, so it is PRESENT and
//!   empty on a base image, in every form of the read;
//! * the recipe an image reports is the published build it came out of: an ARN of the
//!   image-build service that owns the recipe, under the account AWS builds in, naming one
//!   recipe per operating system and accelerator and the revision of it the image's batch
//!   was built from — so two batches of one combination name the SAME recipe at
//!   consecutive revisions, and no image's recipe is named after the image;
//! * an accelerated image reports the NVIDIA GRID driver its batch installed, and an image
//!   with no such driver reports no software metadata at all;
//! * AWS publishes an accelerated base image per graphics accelerator for every operating
//!   system it accelerates, so a read addressing one is answered rather than refused;
//! * an image built with the GPU vendor's generic driver states every accelerated family it
//!   runs on, while the family it reports as its own is the one its hardware is.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::images::{catalog, describe, published_image_arn, reported_names};
use crate::stack_fleet_lifecycle::Emulator;

/// The ARN `service` segment of a published base-image recipe: the recipe belongs to the
/// AWS image-build service that produces the published AMIs, not to AppStream.
const RECIPE_SERVICE: &str = "amifarm";

/// The account the published recipes are owned by — AWS builds its base images in its own
/// account, so the provenance ARN names that account and not the caller's.
const RECIPE_PUBLISHER_ACCOUNT: &str = "121014824124";

/// The clause the published description of a general-purpose base image ends in.
const GENERAL_LAUNCH_CLAUSE: &str = "image for launching image builder instances";

/// The graphics accelerators the catalog publishes an accelerated base image per
/// (`GRAPHICS_ACCELERATORS` in `src/state/images.rs`, the tokens AWS's accelerated
/// base-image names carry after `Graphics-`).
const ACCELERATORS: &[&str] = &["G4dn", "G5", "G6", "G7", "NV"];

/// Release batches the catalog publishes, `MM-DD-YYYY` as a published name carries the date
/// (`PUBLISHED_RELEASE_BATCHES` in `src/state/images.rs`).
const BATCH_DATES: &[&str] = &["11-10-2025", "02-18-2026", "04-16-2026"];

/// The release date a published image's name ends in, `(year, month, day)`.
type BatchDate = (String, String, String);

/// The published build an image came out of: the recipe's name and its revision.
type Recipe = (String, u32);

/// One recipe line: the build each release batch of one operating-system-and-accelerator
/// combination came out of, keyed by the batch's date so the entries walk in release order.
type RecipeLine = BTreeMap<BatchDate, Recipe>;

/// The `Name` of one reported image.
fn name_of(image: &Value) -> String {
    image["Name"].as_str().unwrap_or("<no Name>").to_string()
}

/// The accelerator token a published name carries after `Graphics-`, and `None` for a
/// general-purpose image.
fn accelerator_of(name: &str) -> Option<String> {
    name.strip_prefix("AppStream-Graphics-")
        .and_then(|tail| tail.split('-').next())
        .map(str::to_string)
}

/// The release date a published image's name ends in, `(year, month, day)`.
fn batch_of(name: &str) -> BatchDate {
    // `rsplitn` walks from the end, so the fields are the year, the day and the month.
    let tail: Vec<&str> = name.rsplitn(4, '-').collect();
    assert!(
        tail.len() == 4,
        "case {name:?}: a published name ends in its release date MM-DD-YYYY"
    );
    (tail[0].to_string(), tail[2].to_string(), tail[1].to_string())
}

/// The whole catalog of one Region, keyed by image name.
async fn by_name(emulator: &Emulator) -> BTreeMap<String, Value> {
    catalog(emulator)
        .await
        .into_iter()
        .map(|image| (name_of(&image), image))
        .collect()
}

/// One image the catalog must publish, read through the front door by name.
async fn published(emulator: &Emulator, case: &str, name: &str) -> Value {
    let (images, _) = describe(emulator, case, json!({ "Names": [name] })).await;
    assert_eq!(
        reported_names(&images),
        vec![name.to_string()],
        "case {case:?}: expected exactly the published image {name:?}, got {images:?}"
    );
    images[0].clone()
}

#[tokio::test]
async fn a_published_image_states_the_description_aws_words_for_it() {
    let emulator = Emulator::in_region("us-west-2");

    // Rows: one published image per wording the catalog carries — the two Linux releases
    // AWS words differently, the one that words its general-purpose and its accelerated
    // image differently, and an operating system and a batch no recorded request named.
    let rows: &[(&str, &str)] = &[
        (
            "AppStream-RHEL8-11-10-2025",
            "AppStream redhat linux (RHEL 8) image for launching image builder instances",
        ),
        (
            "AppStream-Graphics-G4dn-RHEL8-02-18-2026",
            "AppStream redhat linux image (RHEL 8) for launching Graphics G4dn image builder \
             instances",
        ),
        (
            "AppStream-RockyLinux8-11-10-2025",
            "AppStream Rocky Linux 8 image for launching image builder instances",
        ),
        (
            "AppStream-Graphics-G4dn-RockyLinux8-04-16-2026",
            "AppStream Rocky Linux 8 image for launching Graphics G4dn image builder instances",
        ),
        (
            "AppStream-AmazonLinux2-09-05-2025",
            "AppStream amazon linux 2 image for launching image builder instances",
        ),
        (
            "AppStream-Graphics-G6-WinServer2022-08-31-2026",
            "AppStream Windows Server 2022 image for launching Graphics G6 image builder \
             instances",
        ),
    ];

    for (name, expected) in rows {
        let image = published(&emulator, name, name).await;
        assert_eq!(
            image["Description"].as_str(),
            Some(*expected),
            "case {name:?}: expected Description {expected:?}, got {image}"
        );
    }

    // Every image of the catalog states the published launch clause of its kind: an
    // accelerated image names the accelerator the instances it launches carry, a
    // general-purpose one names none.
    for (name, image) in by_name(&emulator).await {
        let description = image["Description"].as_str().unwrap_or_default().to_string();
        let expected_tail = match accelerator_of(&name) {
            Some(accelerator) => {
                format!("for launching Graphics {accelerator} image builder instances")
            }
            None => GENERAL_LAUNCH_CLAUSE.to_string(),
        };
        assert!(
            description.ends_with(&expected_tail),
            "case {name:?}: expected a description ending {expected_tail:?}, got \
             {description:?}"
        );
        assert!(
            description.starts_with("AppStream "),
            "case {name:?}: a published description names the service it is published by, \
             got {description:?}"
        );
    }
}

#[tokio::test]
async fn every_image_states_its_instance_type_groups_in_every_form_of_the_read() {
    let region = "eu-west-1";
    let emulator = Emulator::in_region(region);
    let catalog = by_name(&emulator).await;
    let names: Vec<String> = catalog.keys().cloned().collect();

    // Rows: every image of the whole-catalog read. A base image serves no instance type
    // group of its own and says so with an empty list, so code that walks the groups of
    // every reported image never meets a missing member.
    for (name, image) in &catalog {
        assert_eq!(
            image["SupportedInstanceTypeGroups"],
            json!([]),
            "case {name:?}: expected SupportedInstanceTypeGroups [], got {image}"
        );
    }

    // Rows: the other forms of the read — by name, by ARN, by visibility — answered for a
    // general-purpose image and for an accelerated one. The member is the record's, so
    // every form reports it.
    let general = names
        .iter()
        .find(|name| accelerator_of(name).is_none())
        .expect("the catalog publishes a general-purpose image")
        .clone();
    let graphics = names
        .iter()
        .find(|name| accelerator_of(name).is_some())
        .expect("the catalog publishes an accelerated image")
        .clone();
    for name in [general, graphics] {
        let rows: Vec<(String, Value)> = vec![
            (format!("Names [{name:?}]"), json!({ "Names": [name] })),
            (
                format!("Arns [{name:?}]"),
                json!({ "Arns": [published_image_arn(region, &name)] }),
            ),
            (
                format!("Names [{name:?}] of PUBLIC"),
                json!({ "Names": [name], "Type": "PUBLIC" }),
            ),
        ];
        for (case, request) in rows {
            let (images, _) = describe(&emulator, &case, request).await;
            for image in &images {
                assert_eq!(
                    image["SupportedInstanceTypeGroups"],
                    json!([]),
                    "case {case:?}: expected SupportedInstanceTypeGroups [], got {image}"
                );
            }
        }
    }
}

#[tokio::test]
async fn an_image_reports_the_recipe_revision_it_was_built_from() {
    // Rows: the Region the request is signed for — the recipe is addressed in the Region
    // the request asks about, like the image itself.
    for region in ["us-west-2", "ap-southeast-2"] {
        let emulator = Emulator::in_region(region);

        // Rows: a published image per recipe line — general-purpose and accelerated, two
        // operating systems, two consecutive batches of one combination, and a
        // combination and a batch no recorded request named.
        let rows: &[(&str, &str, u32)] = &[
            ("AppStream-RHEL8-11-10-2025", "AppStreamRHEL8NonGraphicsRecipe", 10),
            (
                "AppStream-Graphics-G4dn-RHEL8-02-18-2026",
                "AppStreamRHEL8GraphicsG4Recipe",
                10,
            ),
            (
                "AppStream-Graphics-G4dn-RHEL8-04-16-2026",
                "AppStreamRHEL8GraphicsG4Recipe",
                11,
            ),
            (
                "AppStream-RockyLinux8-11-10-2025",
                "AppStreamRockyLinux8NonGraphicsRecipe",
                10,
            ),
            (
                "AppStream-Graphics-G4dn-RockyLinux8-02-18-2026",
                "AppStreamRockyLinux8GraphicsG4Recipe",
                10,
            ),
            (
                "AppStream-Graphics-G5-AmazonLinux2-09-22-2026",
                "AppStreamAmazonLinux2GraphicsG5Recipe",
                13,
            ),
            (
                "AppStream-WinServer2025-06-17-2024",
                "AppStreamWinServer2025NonGraphicsRecipe",
                6,
            ),
        ];

        for (name, recipe, revision) in rows {
            let case = format!("{region} {name}");
            let image = published(&emulator, &case, name).await;
            let expected = format!(
                "arn:aws:{RECIPE_SERVICE}:{region}:{RECIPE_PUBLISHER_ACCOUNT}:recipe/{recipe}/\
                 {revision}"
            );
            assert_eq!(
                image["appStreamRecipeArn"].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected appStreamRecipeArn {expected:?}, got {image}"
            );
        }
    }
}

#[tokio::test]
async fn one_recipe_serves_every_batch_of_a_combination_at_consecutive_revisions() {
    let region = "us-west-2";
    let emulator = Emulator::in_region(region);
    let prefix = format!("arn:aws:{RECIPE_SERVICE}:{region}:{RECIPE_PUBLISHER_ACCOUNT}:recipe/");

    // Rows: every published image, grouped by the operating system and the accelerator its
    // recipe is named for. The recipe is the build the images of that combination come out
    // of, so one name serves every batch and the revision advances by one per batch.
    let mut lines: BTreeMap<String, RecipeLine> = BTreeMap::new();
    for (name, image) in by_name(&emulator).await {
        let arn = image["appStreamRecipeArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {name:?}: an image states its recipe, got {image}"))
            .to_string();
        let tail = arn.strip_prefix(&prefix).unwrap_or_else(|| {
            panic!("case {name:?}: expected a recipe ARN under {prefix:?}, got {arn:?}")
        });
        let (recipe, revision) = tail.rsplit_once('/').unwrap_or_else(|| {
            panic!("case {name:?}: a recipe ARN ends in the revision, got {arn:?}")
        });
        let revision: u32 = revision.parse().unwrap_or_else(|_| {
            panic!("case {name:?}: expected a numbered revision, got {revision:?}")
        });
        assert!(
            !recipe.contains(name.as_str()) && !recipe.contains('-'),
            "case {name:?}: the recipe is named for the build and not after the image, \
             got {recipe:?}"
        );
        let platform = image["Platform"].as_str().unwrap_or_default().to_string();
        let family = image["InstanceFamily"].as_str().unwrap_or_default().to_string();
        lines
            .entry(format!("{platform}|{family}"))
            .or_default()
            .insert(batch_of(&name), (recipe.to_string(), revision));
    }
    assert!(
        lines.len() > 1,
        "case \"the published catalog\": every combination has its own recipe, got {:?}",
        lines.keys().collect::<Vec<_>>()
    );

    for (combination, batches) in &lines {
        let names: BTreeSet<&String> = batches.values().map(|(recipe, _)| recipe).collect();
        assert_eq!(
            names.len(),
            1,
            "case {combination:?}: one recipe serves every batch of a combination, \
             got {names:?}"
        );
        // The batches are keyed by (year, month, day), so walking the map walks them in
        // release order.
        let revisions: Vec<u32> = batches.values().map(|(_, revision)| *revision).collect();
        for pair in revisions.windows(2) {
            assert_eq!(
                pair[1],
                pair[0] + 1,
                "case {combination:?}: a recipe is revised once per batch, got {revisions:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_accelerated_image_states_the_gpu_driver_its_batch_installed() {
    let emulator = Emulator::in_region("us-west-2");

    // Rows: every published image. The model's
    // `ImageSoftwareMetadata$nvidiaGridDriverVersion` doc: "The version of the NVIDIA GRID
    // driver installed on the image. This field is empty if no NVIDIA GRID driver is
    // installed" — so an image that states a driver version is an accelerated one, and the
    // version is of the form the member's `@pattern ^[0-9]+(\.[0-9]+){1,2}$` admits, within
    // its `length 1..32`.
    let mut accelerated_with_driver = 0_usize;
    for (name, image) in by_name(&emulator).await {
        if accelerator_of(&name).is_none() {
            assert!(
                image.get("ImageSoftwareMetadata").is_none(),
                "case {name:?}: an image with no GPU driver installed states no software \
                 metadata, got {image}"
            );
            continue;
        }
        if let Some(metadata) = image.get("ImageSoftwareMetadata") {
            let version = metadata["nvidiaGridDriverVersion"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {name:?}: expected a driver version, got {metadata}")
                });
            let fields: Vec<&str> = version.split('.').collect();
            assert!(
                (2..=3).contains(&fields.len())
                    && version.len() <= 32
                    && fields.iter().all(|field| !field.is_empty()
                        && field.chars().all(|digit| digit.is_ascii_digit())),
                "case {name:?}: expected a GRID driver version of the modelled form, \
                 got {version:?}"
            );
            accelerated_with_driver += 1;
        }
    }
    assert!(
        accelerated_with_driver > 0,
        "case \"the published catalog\": the batches that install the GRID driver publish \
         its version, got none"
    );
}

#[tokio::test]
async fn every_accelerated_operating_system_publishes_an_image_per_accelerator() {
    let region = "us-west-2";
    let emulator = Emulator::in_region(region);

    // Rows: every accelerator, under every batch below, for the operating system whose
    // accelerated images the catalog must publish. Each is a read AWS answers with the
    // image, by name and by the ARN the catalog reports — not with a refusal.
    for accelerator in ACCELERATORS {
        for date in BATCH_DATES {
            let name = format!("AppStream-Graphics-{accelerator}-RockyLinux8-{date}");
            let case = format!("{name} by name");
            let image = published(&emulator, &case, &name).await;

            let rows: Vec<(&str, Value)> = vec![
                ("Platform", json!("ROCKY_LINUX8")),
                ("Visibility", json!("PUBLIC")),
                ("State", json!("AVAILABLE")),
                ("ImageBuilderSupported", json!(true)),
                ("SupportedInstanceTypeGroups", json!([])),
            ];
            for (member, expected) in rows {
                assert_eq!(
                    image[member], expected,
                    "case {case:?}: expected {member} {expected}, got {image}"
                );
            }
            assert!(
                image["SupportedInstanceFamilies"]
                    .as_array()
                    .is_some_and(|families| !families.is_empty()),
                "case {case:?}: an accelerated image states the families it runs on, \
                 got {image}"
            );

            let case = format!("{name} by ARN");
            let arn = published_image_arn(region, &name);
            let (images, _) = describe(&emulator, &case, json!({ "Arns": [arn.clone()] })).await;
            assert_eq!(
                images.first().and_then(|image| image["Arn"].as_str()),
                Some(arn.as_str()),
                "case {case:?}: expected the image under {arn:?}, got {images:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_image_built_with_the_vendor_driver_runs_on_every_accelerated_family() {
    let emulator = Emulator::in_region("us-west-2");
    let catalog = by_name(&emulator).await;

    let families = |image: &Value| -> Vec<String> {
        image["SupportedInstanceFamilies"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|family| family.as_str().unwrap_or_default().to_string())
            .collect()
    };

    // The families every image built for ONE accelerator states: the accelerated families
    // the catalog publishes images for.
    let mut single: BTreeSet<String> = BTreeSet::new();
    for (name, image) in &catalog {
        if accelerator_of(name).is_some() {
            let stated = families(image);
            if stated.len() == 1 {
                single.insert(stated[0].clone());
            }
        }
    }
    assert!(
        single.len() > 1,
        "case \"the published catalog\": the catalog publishes images for more than one \
         accelerated family, got {single:?}"
    );

    // Rows: every image whose accelerator is the GPU vendor's generic one. It is built with
    // the driver of every accelerated family, so it states them all — more than one — while
    // the family it reports as its own is its hardware's.
    let mut vendor_images = 0_usize;
    for (name, image) in &catalog {
        if accelerator_of(name).as_deref() != Some("NV") {
            continue;
        }
        vendor_images += 1;
        let stated = families(image);
        assert_eq!(
            stated.iter().cloned().collect::<BTreeSet<String>>(),
            single,
            "case {name:?}: expected every accelerated family {single:?}, got {stated:?}"
        );
        assert_eq!(
            image["InstanceFamily"].as_str(),
            Some("GRAPHICS_NV"),
            "case {name:?}: the family an image reports as its own is its hardware's, \
             got {image}"
        );
    }
    assert!(
        vendor_images > 0,
        "case \"the published catalog\": the vendor-driver image must be published, got none"
    );
}
