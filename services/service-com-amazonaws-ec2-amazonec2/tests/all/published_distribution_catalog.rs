// SPDX-License-Identifier: CC0-1.0

//! Resolving a NON-Amazon-Linux operating system out of the published AMI catalog, over the
//! wire the service speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered
//! with XML; README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: THE PUBLIC CATALOG HOLDS A CURRENT BUILD OF EVERY THIRD-PARTY
//! ENTERPRISE LINUX DISTRIBUTION RELEASE, VARIANT AND ARCHITECTURE AWS PUBLISHES UNDER THE
//! `amazon` OWNER ALIAS — so a `DescribeImages` narrowed to that owner, a vendor's published
//! name pattern and an architecture reports at least one public, `available` image carrying
//! the `ImageId` and `CreationDate` a caller sorts on, and the newest match launches. That is
//! how a template or an infrastructure-as-code app that pins a distribution image resolves its
//! `ImageId`; `DescribeImages` models no error for a filter that selects nothing, so a release
//! the catalog does not hold is an empty list the client cannot tell from "this Region
//! publishes none" and the deploy fails silently.
//!
//! The rows span what that generalises over rather than the one release a check pins: both
//! vendors the catalog publishes ([`ec2::state::published_distribution_specs`]), both
//! architectures of each, the previous release beside the current one, and the SAP variant —
//! each by the name pattern a client writes rather than by an AMI id, which is per-Region and
//! per-release. The second test takes its rows from the catalog itself, so a release added
//! later is covered without touching the table, and reads back the billing facts an instance
//! launched from such an image is charged under. The third is the contrast that makes the
//! first an assertion about the catalog rather than about a filter that matches everything.
//!
//! The owner forms, the reading of one reported image, the "newest match" rule and the launch
//! round trip are [`super::published_amazon_linux_catalog`]'s, used from here rather than
//! written twice.

use service_com_amazonaws_ec2_amazonec2 as ec2;

use super::ec2_query::{as_timestamp, call, element, elements, filters, indexed};
use super::published_amazon_linux_catalog::{
    described, launch_and_read_back, newest, OwnerForm, AMAZON, AVAILABLE, OWNER_FORMS,
};
use super::wire::Param;

/// The Region every call here is signed for; the image catalog is per account.
const REGION: &str = "us-east-1";

/// One published distribution variant to resolve: the name pattern a client matches on, and
/// the architecture that build is published for.
///
/// The patterns are the vendors' own published AMI name forms, as the catalog records them
/// (`suse-sles[-sap]-<release>-v<build date>-hvm-ssd-<architecture>` and
/// `RHEL-<version>_HVM-<build date>-<architecture>-<build>-Hourly2-GP3`, the name forms
/// documented on `ec2::state::published_distribution_specs`) — not the ids, names or dates of
/// any one release, which are per-Region, per-release values. Rows the reported defect did not
/// cite are here beside the one it did: the other architecture of that release, the previous
/// release of each family, the SAP variant, and the other vendor entirely.
const PUBLISHED_VARIANTS: &[(&str, &str)] = &[
    // SUSE Linux Enterprise Server: the current major release in both architectures, the
    // previous one in both, and the SAP Applications variant.
    ("suse-sles-16-0-*", "x86_64"),
    ("suse-sles-16-0-*", "arm64"),
    ("suse-sles-15-sp7-*", "x86_64"),
    ("suse-sles-15-sp7-*", "arm64"),
    ("suse-sles-sap-16-0-*", "x86_64"),
    // Red Hat Enterprise Linux, whose names spell a release and a build the other way round.
    ("RHEL-10*", "x86_64"),
    ("RHEL-10*", "arm64"),
    ("RHEL-9.6*", "x86_64"),
    ("RHEL-9*-arm64-*", "arm64"),
];

/// Name patterns and architectures the catalog publishes no build of: a SUSE release and a Red
/// Hat release whose support ended long before the catalog's builds, and an architecture token
/// no published distribution name carries.
const UNPUBLISHED_VARIANTS: &[(&str, &str)] = &[
    ("suse-sles-11-*", "x86_64"),
    ("RHEL-7.*", "x86_64"),
    ("suse-sles-16-0-*", "i386"),
];

/// The request a client pinning a distribution image sends: the `amazon` owner, the vendor's
/// name pattern, the architecture it launches on and the `available` state.
fn resolve(form: OwnerForm, pattern: &str, architecture: &str) -> Vec<Param> {
    let patterns = [pattern];
    let architectures = [architecture];
    let mut entries: Vec<(&str, &[&str])> = Vec::new();
    if form == OwnerForm::OwnerAliasFilter {
        entries.push(("owner-alias", &[AMAZON]));
    }
    entries.push(("name", &patterns));
    entries.push(("architecture", &architectures));
    entries.push(("state", &[AVAILABLE]));
    let mut params = filters(&entries);
    if form == OwnerForm::OwnersMember {
        params.extend(indexed("Owner", &[AMAZON]));
    }
    params
}

/// Every published distribution variant resolves to at least one Amazon-aliased, available
/// image of the architecture asked for — whichever documented way the request names that
/// owner — and the newest match launches.
#[tokio::test]
async fn every_published_distribution_variant_resolves_to_a_launchable_image() {
    let (router, _engine) = super::ec2_query::emulator();

    for (pattern, architecture) in PUBLISHED_VARIANTS {
        for form in OWNER_FORMS {
            let case = format!("{pattern} on {architecture} via {}", form.label());
            let reported = described(&case, &router, &resolve(form, pattern, architecture)).await;
            assert!(
                !reported.is_empty(),
                "case {case:?}: expected at least one published available image of that \
                 distribution release, got an empty set"
            );

            for image in &reported {
                assert!(
                    image.image_id.starts_with("ami-"),
                    "case {case:?}: every reported image carries an AMI id, got {image:?}"
                );
                assert_eq!(
                    image.state, AVAILABLE,
                    "case {case:?}: the state filter must hold, got {image:?}"
                );
                assert_eq!(
                    image.owner_alias, AMAZON,
                    "case {case:?}: these images are published under that alias, got {image:?}"
                );
                assert_eq!(
                    image.public, "true",
                    "case {case:?}: a published image is visible to every account, got {image:?}"
                );
                assert_eq!(
                    &image.architecture, architecture,
                    "case {case:?}: the architecture filter must hold, got {image:?}"
                );
                assert!(
                    as_timestamp(&image.creation_date).is_some(),
                    "case {case:?}: the creation date a caller sorts on must be an instant, got \
                     {image:?}"
                );
                assert_eq!(
                    image.deprecation_time, None,
                    "case {case:?}: a current build of a published release is not deprecated, \
                     got {image:?}"
                );
            }

            // The documented way to pick one: the newest `CreationDate` of the matches. It is
            // launched once per row, the launch being about the id rather than about which way
            // the request named its owner.
            let picked = newest(&case, &reported);
            if form == OwnerForm::OwnersMember {
                let ran = launch_and_read_back(&case, &router, &picked.image_id).await;
                assert_eq!(
                    ran, picked.image_id,
                    "case {case:?}: the instance must report the AMI it was launched from, got \
                     {ran:?}"
                );
            }
        }
    }
}

/// The same rule read off the catalog itself rather than off the table above: every build the
/// store publishes for one of these distributions is reported under its own published name,
/// with the architecture, creation date and description that build carries and the billing
/// facts an instance launched from it is charged under — and reporting it twice reports the
/// same thing.
#[tokio::test]
async fn every_published_distribution_build_is_reported_with_its_own_facts() {
    let (router, _engine) = super::ec2_query::emulator();

    let published = ec2::state::published_distribution_specs();
    assert!(
        !published.is_empty(),
        "the catalog must publish a build of the distributions AWS publishes beside its own"
    );

    for spec in &published {
        let case = spec.name.clone();
        let by_name = filters(&[("name", &[case.as_str()])]);
        let (status, body) = call(&router, REGION, "DescribeImages", &by_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeImages, got {status} with body {body}"
        );
        let reported_names = elements(&body, "name");
        assert_eq!(
            reported_names,
            vec![case.clone()],
            "case {case:?}: its own published name must report exactly that build, got \
             {reported_names:?}"
        );

        for (member, expected) in [
            ("architecture", spec.architecture.clone()),
            ("creationDate", spec.creation_date.clone()),
            ("description", spec.description.clone()),
            ("imageOwnerAlias", AMAZON.to_string()),
            ("imageState", AVAILABLE.to_string()),
            ("isPublic", "true".to_string()),
            ("platformDetails", spec.platform_details.clone()),
            ("usageOperation", spec.usage_operation.clone()),
        ] {
            let reported = element(&body, member);
            assert_eq!(
                reported.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected {member} {expected:?}, got {reported:?}"
            );
        }

        // The build date the published name carries is the instant the image reports as its
        // creation date, which is what makes "the newest match of a name pattern" pick the same
        // build on every run.
        let build_date: String = spec
            .creation_date
            .chars()
            .take("YYYY-MM-DD".len())
            .filter(char::is_ascii_digit)
            .collect();
        assert!(
            spec.name.contains(&build_date),
            "case {case:?}: the published name must carry the build date {build_date:?} its \
             creation date {:?} reports",
            spec.creation_date
        );

        // Reporting is a read: the second call answers the same build.
        let (second_status, second_body) = call(&router, REGION, "DescribeImages", &by_name).await;
        assert_eq!(
            second_status, 200,
            "case {case:?}: the second read must succeed too, got {second_status} with body \
             {second_body}"
        );
        assert_eq!(
            elements(&second_body, "imageId"),
            elements(&body, "imageId"),
            "case {case:?}: a read must not change what a later caller sees"
        );
    }
}

/// A release or an architecture the catalog publishes no build of reports an EMPTY set with
/// the same HTTP 200 — the contrast that makes the resolutions above assertions about the
/// catalog rather than about a filter that matches everything.
#[tokio::test]
async fn a_release_the_catalog_does_not_publish_reports_an_empty_set() {
    let (router, _engine) = super::ec2_query::emulator();

    for (pattern, architecture) in UNPUBLISHED_VARIANTS {
        for form in OWNER_FORMS {
            let case = format!("{pattern} on {architecture} via {}", form.label());
            let reported = described(&case, &router, &resolve(form, pattern, architecture)).await;
            assert!(
                reported.is_empty(),
                "case {case:?}: the catalog publishes no such build, so the report must be \
                 empty, got {reported:?}"
            );
        }
    }
}
