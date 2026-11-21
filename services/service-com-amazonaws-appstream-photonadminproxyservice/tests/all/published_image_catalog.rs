// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the AWS-PUBLISHED base images the image catalog read serves.
//!
//! Every case drives the real awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than over one
//! recorded request:
//!
//! * the catalog a client enumerates carries a published base image for every operating
//!   system this API models a current image platform for, and — for an operating system
//!   with accelerated images — one per graphics instance family, so every instance family
//!   a fleet can be launched on has an image to launch;
//! * every published image is named the way AWS names its base images,
//!   `AppStream-[Graphics-<accelerator>-]<operating system>-<MM-DD-YYYY>`, and reports
//!   that name as the name to display;
//! * a published image states exactly the instance families its hardware serves: the one
//!   accelerated family of a graphics image, the general-purpose families otherwise;
//! * a published image is owned by no account, so its ARN names the Region the request
//!   was signed for and an EMPTY account — and the same ARN under an account addresses no
//!   image;
//! * every form of the read — unfiltered, by name, by ARN, by visibility and paged — is
//!   answered with an `Images` list and never with a not-found refusal, and a name that is
//!   in the catalog is answered with exactly that image.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::images::{catalog as published_catalog, describe, image_arn, published_image_arn};
use crate::refusal_messages::{not_found_sentence, refusal};
use crate::stack_fleet_lifecycle::Emulator;

/// The word AWS's refusal sentences use for an image.
const IMAGE_KIND: &str = "image";

/// The `VisibilityType` of an image AWS publishes
/// (`smithy_query(query="shape", shape="VisibilityType")`: PRIVATE | PUBLIC | SHARED).
const PUBLIC: &str = "PUBLIC";

/// Every operating system this API models an image platform for
/// (`smithy_query(query="shape", shape="PlatformType")`), paired with the token AWS's
/// image names carry for it. `WINDOWS` and `WINDOWS_SERVER_2016` are the platforms of
/// images published before the shipped snapshot, so no current base image carries them.
const PLATFORM_TOKENS: &[(&str, &str)] = &[
    ("AMAZON_LINUX2", "AmazonLinux2"),
    ("RHEL8", "RHEL8"),
    ("ROCKY_LINUX8", "RockyLinux8"),
    ("UBUNTU_PRO_2404", "UbuntuPro2404"),
    ("WINDOWS_SERVER_2019", "WinServer2019"),
    ("WINDOWS_SERVER_2022", "WinServer2022"),
    ("WINDOWS_SERVER_2025", "WinServer2025"),
];

/// The accelerated instance families the model's `Image$SupportedInstanceFamilies` doc
/// enumerates (`smithy_query(query="doc", shape="Image",
/// member="SupportedInstanceFamilies")`: General Purpose, Compute Optimized, Memory
/// Optimized, Graphics G4, Graphics G5, Graphics G6).
const GRAPHICS_FAMILIES: &[&str] = &["Graphics G4", "Graphics G5", "Graphics G6"];

/// The general-purpose families of that same enumeration — the ones a non-accelerated
/// image serves.
const GENERAL_FAMILIES: &[&str] = &["General Purpose", "Compute Optimized", "Memory Optimized"];

/// The catalog as a client reads it: every published image the sibling suite's whole-catalog
/// read reports (`crate::images::catalog`), keyed by its `Name` so a row can look one up.
async fn catalog(emulator: &Emulator, case: &str) -> BTreeMap<String, Value> {
    published_catalog(emulator)
        .await
        .into_iter()
        .map(|image| {
            let name = image["Name"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: every image states a Name, got {image}"))
                .to_string();
            (name, image)
        })
        .collect()
}

/// The instance families one reported image states support for.
fn families(image: &Value) -> Vec<String> {
    image["SupportedInstanceFamilies"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|family| family.as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn the_catalog_carries_a_published_image_for_every_platform_and_instance_family() {
    let emulator = Emulator::new();
    let catalog = catalog(&emulator, "the published catalog").await;
    let names: Vec<&String> = catalog.keys().collect();

    // Rows: every modelled platform token. Each must have a general-purpose base image,
    // named `AppStream-<operating system>-<MM-DD-YYYY>`.
    for (platform, token) in PLATFORM_TOKENS {
        let case = format!("the general-purpose image of {platform}");
        let image = catalog
            .values()
            .find(|image| {
                image["Platform"].as_str() == Some(platform)
                    && image["Name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with(&format!("AppStream-{token}-")))
            })
            .unwrap_or_else(|| {
                panic!("case {case:?}: the catalog must publish one, got {names:?}")
            });
        assert_eq!(
            families(image),
            GENERAL_FAMILIES
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<String>>(),
            "case {case:?}: a non-accelerated image serves the general-purpose families, \
             got {image}"
        );
    }

    // Rows: every accelerated instance family. Each must have at least one published
    // graphics image, whose name carries `Graphics-<accelerator>` and which states that
    // ONE family and no other.
    for family in GRAPHICS_FAMILIES {
        let case = format!("the graphics images of {family}");
        let serving: Vec<&Value> = catalog
            .values()
            .filter(|image| families(image) == vec![(*family).to_string()])
            .collect();
        assert!(
            !serving.is_empty(),
            "case {case:?}: the catalog must publish an image a fleet of this family can \
             launch, got {names:?}"
        );
        for image in serving {
            let name = image["Name"].as_str().unwrap_or_default();
            assert!(
                name.starts_with("AppStream-Graphics-"),
                "case {case:?}: an accelerated image is named for its accelerator, got {name:?}"
            );
            assert_eq!(
                families(image).len(),
                1,
                "case {case:?}: an accelerated image serves its own family only, got {image}"
            );
        }
    }

    // Every published image's name carries the release date of the batch it belongs to,
    // written `MM-DD-YYYY`, and the name is one a fleet can name: `ImageName` matches
    // `^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,100}$`
    // (`smithy_query(query="operation", operation="CreateFleet")`).
    for (name, image) in &catalog {
        let case = format!("the name {name:?}");
        // `rsplitn` walks from the end, so the fields are the year, the day, the month and
        // then the `AppStream-[Graphics-<accelerator>-]<operating system>` head.
        let tail: Vec<&str> = name.rsplitn(4, '-').collect();
        assert!(
            tail.len() == 4
                && tail[0].len() == 4
                && tail[1].len() == 2
                && tail[2].len() == 2
                && tail[..3]
                    .iter()
                    .all(|field| field.chars().all(|digit| digit.is_ascii_digit())),
            "case {case:?}: a published name ends in its release date MM-DD-YYYY"
        );
        assert!(
            name.starts_with("AppStream-")
                && (1..=101).contains(&name.len())
                && name.chars().all(|letter| letter.is_ascii_alphanumeric()
                    || ['_', '.', '-'].contains(&letter)),
            "case {case:?}: the name must be one a fleet can name"
        );
        assert_eq!(
            image["DisplayName"].as_str(),
            Some(name.as_str()),
            "case {case:?}: AWS publishes the image under its own name, got {image}"
        );
        assert_eq!(
            image["Visibility"].as_str(),
            Some(PUBLIC),
            "case {case:?}: an AWS-published image is public, got {image}"
        );
    }
}

#[tokio::test]
async fn a_published_image_is_addressed_by_an_arn_that_names_no_account() {
    // Rows: the Region the request is signed for. The published catalog is the same in
    // every Region, and each image reports that Region's own accountless ARN.
    for region in ["us-east-1", "eu-west-1", "ap-southeast-2"] {
        let emulator = Emulator::in_region(region);
        let catalog = catalog(&emulator, region).await;

        for (name, image) in &catalog {
            let case = format!("{region} {name}");
            let expected = published_image_arn(region, name);
            assert_eq!(
                image["Arn"].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected the accountless ARN {expected:?}, got {image}"
            );
        }

        // Rows: an image addressed by the ARN it reports, which answers that image; and
        // the same ARN with this account in it, which addresses no image at all.
        for name in catalog.keys() {
            let case = format!("{region} {name} by its own ARN");
            let (images, _) = describe(
                &emulator,
                &case,
                json!({ "Arns": [published_image_arn(region, name)] }),
            )
            .await;
            assert_eq!(
                images.first().and_then(|image| image["Name"].as_str()),
                Some(name.as_str()),
                "case {case:?}: expected exactly that image, got {images:?}"
            );

            let case = format!("{region} {name} under an account");
            let message = refusal(
                &emulator,
                &case,
                "DescribeImages",
                json!({ "Arns": [image_arn(region, name)] }),
                400,
                "ResourceNotFoundException",
            )
            .await;
            let expected = not_found_sentence(IMAGE_KIND, name);
            assert_eq!(
                message, expected,
                "case {case:?}: expected message {expected:?}, got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn every_form_of_the_read_is_answered_from_the_catalog() {
    let region = "us-east-1";
    let emulator = Emulator::in_region(region);
    let catalog = catalog(&emulator, "the published catalog").await;
    let names: Vec<String> = catalog.keys().cloned().collect();
    let graphics: Vec<String> = catalog
        .keys()
        .filter(|name| name.starts_with("AppStream-Graphics-"))
        .cloned()
        .collect();
    assert!(
        !graphics.is_empty(),
        "case \"the published catalog\": a graphics image must be published, got {names:?}"
    );

    // Rows: the forms of this read — unfiltered, by one name, by a graphics name, by the
    // whole catalog's names, by ARN, by visibility, and one image at a time. Each is a
    // read AWS always answers, so each must carry an Images list and no refusal.
    let mut rows: Vec<(String, Value, Option<Vec<String>>)> = vec![
        ("no filters".to_string(), json!({}), Some(names.clone())),
        (
            "by every name".to_string(),
            json!({ "Names": names.clone() }),
            Some(names.clone()),
        ),
        (
            "by visibility".to_string(),
            json!({ "Type": PUBLIC }),
            Some(names.clone()),
        ),
        ("one image at a time".to_string(), json!({ "MaxResults": 1 }), None),
    ];
    for name in &graphics {
        rows.push((
            format!("by the graphics name {name:?}"),
            json!({ "Names": [name] }),
            Some(vec![name.clone()]),
        ));
        rows.push((
            format!("by the graphics ARN of {name:?}"),
            json!({ "Arns": [published_image_arn(region, name)] }),
            Some(vec![name.clone()]),
        ));
    }

    for (case, request, expected) in rows {
        let (page, next_token) = describe(&emulator, &case, request).await;
        let reported: Vec<String> = crate::images::reported_names(&page);
        match expected {
            Some(expected) => assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            ),
            // A page of one: the first image of the catalog, with more to come.
            None => {
                assert_eq!(
                    reported,
                    names[..1].to_vec(),
                    "case {case:?}: expected the first image, got {reported:?}"
                );
                assert!(
                    next_token.is_some(),
                    "case {case:?}: a page smaller than the catalog continues, got none"
                );
            }
        }
    }

    // A walk of the whole catalog one image at a time reports every published image
    // exactly once, whatever page size is asked for.
    for page_size in [1_usize, 4, 25] {
        let case = format!("MaxResults {page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<Value> = None;
        loop {
            let mut request = json!({ "MaxResults": page_size });
            if let Some(token) = token.clone() {
                request["NextToken"] = token;
            }
            let (page, next_token) = describe(&emulator, &case, request).await;
            assert!(
                !page.is_empty() && page.len() <= page_size,
                "case {case:?}: a page holds between one and {page_size} images, got {}",
                page.len()
            );
            seen.extend(crate::images::reported_names(&page));
            token = next_token;
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen.iter().cloned().collect::<BTreeSet<String>>(),
            names.iter().cloned().collect::<BTreeSet<String>>(),
            "case {case:?}: the walk must report every published image, got {seen:?}"
        );
        assert_eq!(
            seen.len(),
            names.len(),
            "case {case:?}: no image may be reported twice, got {seen:?}"
        );
    }
}

#[tokio::test]
async fn a_published_image_a_client_picked_can_be_launched_by_a_fleet() {
    let region = "us-east-1";
    let emulator = Emulator::in_region(region);
    let catalog = catalog(&emulator, "the published catalog").await;

    // Rows: a general-purpose image and a graphics image — what the read reports must be
    // what a fleet can be created with and read back, by name and by the ARN the read
    // reported.
    let general = catalog
        .keys()
        .find(|name| !name.starts_with("AppStream-Graphics-"))
        .expect("the catalog publishes a general-purpose image")
        .clone();
    let graphics = catalog
        .keys()
        .find(|name| name.starts_with("AppStream-Graphics-"))
        .expect("the catalog publishes a graphics image")
        .clone();

    for (index, name) in [general, graphics].iter().enumerate() {
        let arn = published_image_arn(region, name);

        let case = format!("a fleet on {name:?} by name");
        let fleet = format!("published-by-name-{index}");
        emulator
            .ok(
                &case,
                "CreateFleet",
                json!({
                    "Name": fleet,
                    "InstanceType": "stream.standard.small",
                    "ImageName": name,
                    "ComputeCapacity": { "DesiredInstances": 1 },
                }),
            )
            .await;
        let read = emulator
            .ok(&case, "DescribeFleets", json!({ "Names": [fleet] }))
            .await;
        assert_eq!(
            read["Fleets"][0]["ImageName"].as_str(),
            Some(name.as_str()),
            "case {case:?}: a later read must report the published image, got {read}"
        );

        let case = format!("a fleet on {name:?} by the ARN the catalog reported");
        let fleet = format!("published-by-arn-{index}");
        emulator
            .ok(
                &case,
                "CreateFleet",
                json!({
                    "Name": fleet,
                    "InstanceType": "stream.standard.small",
                    "ImageArn": arn,
                    "ComputeCapacity": { "DesiredInstances": 1 },
                }),
            )
            .await;
        let read = emulator
            .ok(&case, "DescribeFleets", json!({ "Names": [fleet] }))
            .await;
        assert_eq!(
            read["Fleets"][0]["ImageArn"].as_str(),
            Some(arn.as_str()),
            "case {case:?}: a later read must report the same accountless ARN, got {read}"
        );
    }
}
