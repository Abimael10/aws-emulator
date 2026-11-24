// SPDX-License-Identifier: CC0-1.0

//! The S3 bucket a domain reports its package assets are stored in, over the wire.
//!
//! Every case drives real `restJson1` requests through the `MultiServiceRouter` the
//! production emulator serves with, so what is pinned here is what a CodeArtifact client
//! reads.
//!
//! The rule under test, asserted over a TABLE of Regions and domains rather than one
//! example: `DomainDescription$s3BucketArn` names the bucket the SERVICE stores a Region's
//! package assets in — `arn:aws:s3:::assets-<asset-bucket-account>-<region>`, where
//! `<region>` is the Region the domain was created against and `<asset-bucket-account>` is
//! the service's own 12-digit account for that Region. That account is a property of the
//! Region alone, so it is the same for every domain of a Region, differs from the account
//! that owns the domain (`DomainDescription$owner`, and the account segment of
//! `DomainDescription$arn`), and is reported identically by every operation that answers
//! with a `DomainDescription` — `CreateDomain`, `DescribeDomain` and `DeleteDomain`.
//!
//! The account component's own digits are NOT part of the rule: nothing here asserts a
//! literal account. What is asserted is its form, its independence from the caller, and
//! its stability — across reads, across domains of a Region, and across emulator
//! instances.

use crate::harness::{
    asset_bucket_account, create_domain_in, delete_domain_in, describe_domain_in, domain_bucket,
    domain_name_rows, emulator, ACCOUNT,
};

/// The Regions the table spans: the default partition's four corners, so a bucket name
/// that ignored the Region (or that carried a per-call value) shows up as two Regions
/// agreeing where they must not.
const REGIONS: [&str; 4] = ["us-east-1", "us-west-2", "eu-central-1", "ap-southeast-2"];

/// The account segment of a CodeArtifact ARN — `arn:aws:codeartifact:<region>:<account>:…`.
fn arn_account(arn: &str) -> String {
    arn.split(':').nth(4).unwrap_or_default().to_string()
}

#[tokio::test]
async fn the_asset_bucket_is_the_services_own_for_the_region() {
    // One emulator per Region, because a domain name is unique per account and Region and
    // the names below repeat across Regions on purpose.
    let mut per_region: Vec<(&str, String)> = Vec::new();

    for region in REGIONS {
        let router = emulator();

        // Rows differ along what a per-domain or per-call derivation would have varied
        // over: the name's length (the modelled extremes of `DomainName`, len 2..50), its
        // character classes, and how many domains the account already holds.
        let mut buckets: Vec<String> = Vec::new();
        for (label, domain) in &domain_name_rows() {
            let case = format!("{region} / {label}");

            let created = create_domain_in(&router, region, domain).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected CreateDomain 200, got {created:?}"
            );
            let reported = domain_bucket(&created, &case);
            let bucket_account = asset_bucket_account(&reported, region, &case);

            // The bucket belongs to the service, so its account is neither the account
            // that owns the domain nor the one the domain's own ARN carries.
            let owner = created.body["domain"]["owner"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let domain_arn = created.body["domain"]["arn"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                owner, ACCOUNT,
                "case {case:?}: expected the caller's account as the domain owner, \
                 got {created:?}"
            );
            assert_ne!(
                bucket_account, owner,
                "case {case:?}: the asset bucket account must not be the domain owner, \
                 got {reported:?}"
            );
            assert_ne!(
                bucket_account,
                arn_account(&domain_arn),
                "case {case:?}: the asset bucket account must not be the account of \
                 {domain_arn:?}, got {reported:?}"
            );

            // Every operation that answers with a `DomainDescription` reports the
            // identical string: the read of the live domain, and the delete that answers
            // for the removed one.
            let described = describe_domain_in(&router, region, domain).await;
            assert_eq!(
                (described.status, domain_bucket(&described, &case).as_str()),
                (200, reported.as_str()),
                "case {case:?}: DescribeDomain must report the same bucket, got {described:?}"
            );
            let deleted = delete_domain_in(&router, region, domain).await;
            assert_eq!(
                (deleted.status, domain_bucket(&deleted, &case).as_str()),
                (200, reported.as_str()),
                "case {case:?}: DeleteDomain must report the same bucket, got {deleted:?}"
            );

            // A domain of the same name created again lands in the same Region's bucket:
            // nothing about it is per-call.
            let recreated = create_domain_in(&router, region, domain).await;
            assert_eq!(
                (recreated.status, domain_bucket(&recreated, &case).as_str()),
                (200, reported.as_str()),
                "case {case:?}: a re-created domain must report the same bucket, \
                 got {recreated:?}"
            );

            buckets.push(reported);
        }

        // One bucket per Region, shared by every domain in it.
        assert!(
            buckets.windows(2).all(|pair| pair[0] == pair[1]),
            "case {region:?}: every domain of a Region reports one asset bucket, \
             got {buckets:?}"
        );
        per_region.push((region, buckets.remove(0)));
    }

    // Each Region reports its OWN bucket: no two Regions name one bucket.
    for (left_region, left_bucket) in &per_region {
        for (right_region, right_bucket) in &per_region {
            if left_region == right_region {
                continue;
            }
            assert_ne!(
                left_bucket, right_bucket,
                "case {left_region:?} vs {right_region:?}: each Region has its own asset \
                 bucket, got {left_bucket:?} twice"
            );
        }
    }
}

#[tokio::test]
async fn the_asset_bucket_of_a_region_is_the_same_on_a_fresh_emulator() {
    // The asset bucket of a Region is a service-owned constant, not a value minted per
    // boot: a second emulator reports the same bucket for the same Region.
    for region in REGIONS {
        let case = format!("{region} on a fresh emulator");

        let first = emulator();
        let second = emulator();
        let left = create_domain_in(&first, region, "stable-bucket").await;
        let right = create_domain_in(&second, region, "stable-bucket").await;
        assert_eq!(
            (left.status, right.status),
            (200, 200),
            "case {case:?}: expected CreateDomain 200 on both, got {left:?} and {right:?}"
        );
        assert_eq!(
            domain_bucket(&left, &case),
            domain_bucket(&right, &case),
            "case {case:?}: the Region's asset bucket must not depend on the instance"
        );
        // And the value is not an echo of the request: nothing in it names the domain.
        let reported = domain_bucket(&left, &case);
        assert!(
            !reported.contains("stable-bucket"),
            "case {case:?}: the asset bucket name carries nothing per-domain, \
             got {reported:?}"
        );
    }
}
