// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the AMI a custom DB engine version is built from: the image a
//! `CreateCustomDBEngineVersion` names is resolved against the service that OWNS images.
//!
//! An AMI is not a resource of Amazon RDS — it is Amazon EC2's — so whether the account has
//! the image a CEV would be built from is read off the owner of images and never assumed
//! from the identifier. The rules under test:
//!
//! * a create naming an `ImageId` the calling account owns no image for is refused with the
//!   modelled `Ec2ImagePropertiesNotSupportedFault` (HTTP 400), whose message names the
//!   image and the account that would have to own it;
//! * the image is resolved BEFORE the request's image-source members are judged against each
//!   other, so a create naming an image the account does not have is refused for the image
//!   whatever else it carries — and a create naming an image the account DOES have is then
//!   refused for the members that cannot hold together;
//! * a create naming an image the account owns is served, and the version reads back
//!   carrying that image;
//! * the refusal is grounded in what EC2 ANSWERED: with no owner of images in the process,
//!   nothing is known about the identifier and nothing is refused for it.
//!
//! Amazon EC2 is mounted beside Amazon RDS on ONE engine and driven through the AWS SDK over
//! the router's own in-process callback — the very seam the resolution under test reaches
//! EC2 through (the harness is `db_subnet_group_ec2_zones::Account`, shared rather than
//! copied). Every image id below is one EC2 MINTED or one it minted nothing for, and the
//! account in the expected message is the owner EC2 itself reports for the image.

use crate::custom_db_engine_versions::described;
use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, subtrees, text, wait_for_settle, REGION,
};
use crate::db_subnet_group_ec2_zones::Account;

/// The operation under test.
const ACTION: &str = "CreateCustomDBEngineVersion";

/// The engine every row below creates a version of: RDS Custom for SQL Server, whose
/// `ImageId` documentation states that "for RDS Custom for SQL Server, an AMI ID is required
/// to create a CEV".
const ENGINE: &str = "custom-sqlserver-ee";

/// A well-formed AMI identifier the account registered no image for.
const UNREGISTERED_IMAGE: &str = "ami-0123456789abcdef0";

/// A second one, of the shorter published form, so the rule is asserted over both spellings
/// of an identifier rather than one.
const UNREGISTERED_SHORT_IMAGE: &str = "ami-1a2b3c4d";

/// The root device a registered AMI is described by, which `ec2:RegisterImage` requires of a
/// register that names no Amazon S3 manifest.
const ROOT_DEVICE: &str = "/dev/sda1";

/// The AMI `ec2:RegisterImage` mints for `name`, as the id EC2 reported, and the owner EC2
/// reports for it.
async fn register_image(account: &Account, name: &str) -> (String, String) {
    let image_id = account
        .ec2
        .register_image()
        .name(name)
        .root_device_name(ROOT_DEVICE)
        .send()
        .await
        .unwrap_or_else(|error| panic!("EC2 must register the AMI {name}: {error:?}"))
        .image_id()
        .map(str::to_string)
        .unwrap_or_else(|| panic!("EC2 must report an id for the AMI {name}"));
    let owner = account
        .ec2
        .describe_images()
        .image_ids(&image_id)
        .send()
        .await
        .unwrap_or_else(|error| panic!("EC2 must report the AMI {image_id}: {error:?}"))
        .images()
        .first()
        .and_then(|image| image.owner_id())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("EC2 must report an owner for the AMI {image_id}"));
    (image_id, owner)
}

/// The sentence Amazon RDS answers an `ImageId` the account owns no image for with: the
/// image is not in the account, and the ownership the caller has to check.
fn not_in_account_message(image_id: &str, account: &str) -> String {
    format!(
        "Image {image_id} doesn't exist in your account {account}. Verify {account} is the \
         owner of the EC2 image."
    )
}

/// The parameters of one create: the engine, the CEV name, and whatever the row adds.
fn create_params(engine_version: &str, extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut params = vec![
        ("Engine".to_string(), ENGINE.to_string()),
        ("EngineVersion".to_string(), engine_version.to_string()),
    ];
    for (name, value) in extra {
        params.push(((*name).to_string(), (*value).to_string()));
    }
    params
}

#[tokio::test]
async fn a_custom_engine_version_is_built_only_from_an_ami_the_account_owns() {
    let account = Account::new();
    let (held_image, owner) = register_image(&account, "emu-cev-ami").await;
    let (second_image, _) = register_image(&account, "emu-cev-ami-2").await;

    // The images the account does NOT have, each refused for the image whatever else the
    // request carries: the identifier alone, the shorter spelling of one, and one named
    // beside the member that asks for the service-provided latest AMI — the combination AWS
    // reaches image validation through.
    for (case, engine_version, image_id, extra) in [
        (
            "an AMI the account registered no image for",
            "15.00.4385.2.absent_ami",
            UNREGISTERED_IMAGE,
            Vec::new(),
        ),
        (
            "the shorter spelling of an identifier the account has no image for",
            "15.00.4385.2.absent_short_ami",
            UNREGISTERED_SHORT_IMAGE,
            Vec::new(),
        ),
        (
            "an absent AMI named beside the service-provided latest image",
            "15.00.4385.2.absent_ami_and_latest",
            UNREGISTERED_IMAGE,
            vec![("UseAwsProvidedLatestImage", "true")],
        ),
        (
            "an absent AMI named beside a manifest",
            "15.00.4385.2.absent_ami_and_manifest",
            UNREGISTERED_IMAGE,
            vec![("Manifest", "{\"MediaImportTemplateVersion\":\"2020-08-14\"}")],
        ),
    ] {
        let mut params = vec![("ImageId", image_id)];
        params.extend(extra);
        let body = expect_error(
            case,
            call_in(
                &account.router,
                REGION,
                ACTION,
                &create_params(engine_version, &params),
            )
            .await,
            400,
            "Ec2ImagePropertiesNotSupportedFault",
        );
        let message = text(&body, "Message");
        let expected = not_in_account_message(image_id, &owner);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // An image the account DOES own gets past the image: the request naming it together with
    // the service-provided latest AMI is then refused for the two members, which is the
    // check that comes after.
    let body = expect_error(
        "an AMI the account owns named beside the service-provided latest image",
        call_in(
            &account.router,
            REGION,
            ACTION,
            &create_params(
                "15.00.4385.2.held_ami_and_latest",
                &[
                    ("ImageId", held_image.as_str()),
                    ("UseAwsProvidedLatestImage", "true"),
                ],
            ),
        )
        .await,
        400,
        "InvalidParameterCombination",
    );
    assert!(
        text(&body, "Message").contains("UseAwsProvidedLatestImage"),
        "the refusal must be the one about the two image members, got {body}"
    );

    // And a create naming only an image the account owns is served, and reads back carrying
    // that image.
    for (case, engine_version, image_id) in [
        ("the first AMI the account registered", "15.00.4385.2.held_ami", &held_image),
        ("a second AMI of the same account", "15.00.4385.2.held_ami_2", &second_image),
    ] {
        let created = expect_ok(
            case,
            call_in(
                &account.router,
                REGION,
                ACTION,
                &create_params(engine_version, &[("ImageId", image_id.as_str())]),
            )
            .await,
        );
        let image = subtrees(&created, "Image");
        assert_eq!(
            image.len(),
            1,
            "case {case:?}: the created version must report the one image it was built from, \
             got {created}"
        );
        assert_eq!(
            text(&image[0], "ImageId"),
            *image_id,
            "case {case:?}: the version must carry the AMI the request named, got {created}"
        );
        // The round trip: the version reads back through the report that lists it, carrying
        // the same image. The CEV reaches the status a report lists it under on its own.
        wait_for_settle();
        let described = described(
            &account.router,
            REGION,
            case,
            ENGINE,
            engine_version,
            false,
        );
        assert_eq!(
            described.await.len(),
            1,
            "case {case:?}: the created version must read back through the report"
        );
    }
}

#[tokio::test]
async fn with_no_owner_of_images_in_the_process_no_image_is_refused() {
    // Amazon RDS alone: the hop that would resolve the identifier cannot be made, so this
    // service knows nothing about the image and refuses nothing for it — a refusal has to be
    // grounded in what the owner of images ANSWERED.
    let router = emulator();
    let created = expect_ok(
        "an unresolvable AMI with no EC2 beside this service",
        call(
            &router,
            ACTION,
            &create_params(
                "15.00.4385.2.unresolved_ami",
                &[("ImageId", UNREGISTERED_IMAGE)],
            ),
        )
        .await,
    );
    let image = subtrees(&created, "Image");
    assert_eq!(
        image.len(),
        1,
        "the version must report the image the request named, got {created}"
    );
    assert_eq!(
        text(&image[0], "ImageId"),
        UNREGISTERED_IMAGE,
        "the version must carry the AMI the request named, got {created}"
    );
}
