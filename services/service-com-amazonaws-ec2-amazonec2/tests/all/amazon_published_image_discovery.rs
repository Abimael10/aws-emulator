// SPDX-License-Identifier: CC0-1.0

//! Reading the WHOLE Amazon-published catalog, with no name pattern narrowing it, over the
//! wire the service speaks (`ec2Query`; README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is the other half of [`super::published_amazon_linux_catalog`]'s: the
//! images Amazon publishes are visible to EVERY account under the `amazon` owner alias, and
//! that owner can be asked for both documented ways — named in `Owners`, which takes "a
//! combination of Amazon Web Services account IDs, self, amazon, aws-backup-vault, and
//! aws-marketplace" (`DescribeImagesRequest$Owners`), or selected by the `owner-alias` filter
//! (`DescribeImagesRequest$Filters`). Neither way may see a catalog the other does not, the
//! set is the catalog's own current builds rather than a subset of them, and those images
//! belong to the alias rather than to the calling account.
//!
//! The rows span what that generalises over: both owner forms, read against the whole catalog
//! and against name patterns NO published family carries — a family prefix, a kernel version,
//! a Windows release and an architecture token nothing is published for. That contrast is why
//! the sibling's resolutions are assertions about the catalog rather than about a filter that
//! matches everything: an empty answer is a real negative here and a silent failure there,
//! `DescribeImages` modelling no error for either.
//!
//! The variant table, the two owner forms and the reading of a reported image are the
//! sibling's, used from here rather than written twice.

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec};
use service_com_amazonaws_ec2_amazonec2 as ec2;

use super::ec2_query::{filters, indexed};
use super::published_amazon_linux_catalog::{
    described, resolve_by_name, AMAZON, AVAILABLE, OWNER_FORMS, PUBLISHED_VARIANTS,
};

/// The name of the one image the calling account owns below, so the `self` owner has an image
/// of its own to report beside the published catalog it must not report — an empty answer
/// would say nothing about which images that term selects.
const OWN_IMAGE_NAME: &str = "account-owned-golden-image";

/// Name patterns no published family carries.
///
/// Rows: a family prefix nothing publishes, a kernel version nothing is built on, a Windows
/// release nothing publishes, and an architecture token no Amazon Linux 2 name carries — the
/// published name forms being the sibling's table.
const UNPUBLISHED_PATTERNS: &[&str] = &[
    "amzn4-ami-hvm-*-x86_64-gp2",
    "al2023-ami-*-kernel-9.9-x86_64",
    "Windows_Server-1999-*",
    "amzn2-ami-hvm-*-riscv64-gp2",
];

/// Naming that owner in `Owners` and selecting it with the `owner-alias` filter report the
/// SAME set, and that set is exactly the current builds the catalog publishes.
#[tokio::test]
async fn both_ways_of_naming_the_amazon_owner_report_the_whole_published_catalog() {
    let (router, engine) = super::ec2_query::emulator();
    // One image of the CALLING account, which the alias does not own: the contrast the `self`
    // read below is about.
    let own_id = engine.state::<Ec2State>().lock().insert_image(ImageSpec {
        name: OWN_IMAGE_NAME.to_string(),
        ..ImageSpec::default()
    });

    // The catalog's own current, Amazon-published builds, read off the store's catalog rather
    // than written out here.
    let mut published: Vec<String> = ec2::state::published_image_specs()
        .into_iter()
        .filter(|spec| {
            spec.owner_alias.as_deref() == Some(AMAZON)
                && spec.state == AVAILABLE
                && spec.deprecation_time.is_none()
        })
        .map(|spec| spec.name)
        .collect();
    published.sort();
    assert!(
        published.len() >= PUBLISHED_VARIANTS.len(),
        "the catalog must publish a current build of each of the {} variants the sibling \
         resolves, read {} of them",
        PUBLISHED_VARIANTS.len(),
        published.len()
    );

    // Each form reports exactly the catalog, so neither sees one the other does not, and the
    // account's own image is in neither: it carries no owner alias.
    for form in OWNER_FORMS {
        let case = form.label();
        let reported = described(case, &router, &resolve_by_name(form, None)).await;
        let mut names: Vec<String> = reported.iter().map(|image| image.name.clone()).collect();
        names.sort();
        assert_eq!(
            names, published,
            "case {case:?}: expected every current published build and nothing else, got \
             {names:?}"
        );
    }

    // The published images belong to the alias, not to the calling account, so `self` reports
    // the account's own image and none of them.
    let own = described("Owners=self", &router, &{
        let mut params = filters(&[("state", &[AVAILABLE])]);
        params.extend(indexed("Owner", &["self"]));
        params
    })
    .await;
    let own_names: Vec<String> = own.iter().map(|image| image.name.clone()).collect();
    assert_eq!(
        own_names,
        vec![OWN_IMAGE_NAME.to_string()],
        "case \"Owners=self\": expected the account's own image alone, got {own:?}"
    );
    assert_eq!(
        own[0].image_id, own_id,
        "case \"Owners=self\": expected the id the store minted for it, got {own:?}"
    );
}

/// A name pattern no published family uses reports an EMPTY set with the same HTTP 200,
/// whichever way the request names that owner.
#[tokio::test]
async fn a_name_pattern_no_published_family_uses_reports_an_empty_set() {
    let (router, _engine) = super::ec2_query::emulator();

    for pattern in UNPUBLISHED_PATTERNS {
        for form in OWNER_FORMS {
            let case = format!("{pattern} via {}", form.label());
            let reported = described(&case, &router, &resolve_by_name(form, Some(pattern))).await;
            assert!(
                reported.is_empty(),
                "case {case:?}: no published family carries that name, so the report must be \
                 empty, got {reported:?}"
            );
        }
    }
}
