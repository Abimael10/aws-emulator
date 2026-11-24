// SPDX-License-Identifier: CC0-1.0

//! WHICH account the asset bucket of a Region lives in, over the wire.
//!
//! `tests/all/domain_asset_bucket.rs` pins the FORM of
//! `DomainDescription$s3BucketArn` (`smithy_query(query="shape",
//! shape="DomainDescription")`: "The Amazon Resource Name (ARN) of the Amazon S3 bucket
//! that is used to store package assets in the domain"), its independence from the caller
//! and its stability across reads, domains and instances. This suite pins the account
//! component itself, which that rule leaves open.
//!
//! The rule under test: the account component of a domain's asset-bucket ARN is the
//! SERVICE's own account for the Region the request was addressed to — a value fixed per
//! Region, looked up and never computed from the request — so for a Region whose
//! asset-bucket account the service publishes, every operation that answers with a
//! `DomainDescription` carries exactly that account, for every domain, every call and
//! every emulator instance, and no other Region carries it.
//!
//! Every case drives real `restJson1` requests through the `MultiServiceRouter` the
//! production emulator serves with, so what is pinned here is what a CodeArtifact client
//! reads.

use crate::harness::{
    asset_bucket_account, create_domain_in, delete_domain_in, describe_domain_in, domain_bucket,
    domain_name_rows, emulator, ACCOUNT,
};

/// The asset-bucket accounts the service PUBLISHES, as the bucket name in the `s3BucketArn`
/// a domain of that Region reports: the Region, and the account its asset bucket lives in.
/// Each is a fixed property of its Region, so the emulator must report it verbatim.
const PUBLISHED: [(&str, &str); 1] = [("us-west-2", "787052242323")];

/// Regions the table checks a published account does NOT leak into: the partition's other
/// corners, each of which owns its own asset bucket.
const OTHER_REGIONS: [&str; 4] = ["us-east-1", "eu-central-1", "ap-southeast-2", "sa-east-1"];

#[tokio::test]
async fn a_regions_published_asset_bucket_account_is_reported_verbatim() {
    for (region, published) in PUBLISHED {
        let expected = format!("arn:aws:s3:::assets-{published}-{region}");
        let router = emulator();

        // Rows over what a per-domain derivation would have varied over: nothing about a
        // domain may reach the bucket name, so every row reports the one ARN the Region's
        // published account makes.
        for (label, domain) in &domain_name_rows() {
            let case = format!("{region} / {label}");

            let created = create_domain_in(&router, region, domain).await;
            assert_eq!(
                (created.status, domain_bucket(&created, &case)),
                (200, expected.clone()),
                "case {case:?}: expected CreateDomain to report {expected:?}, got {created:?}"
            );
            // The bucket is the service's: the account the ARN above carries is not the
            // one the same answer reports as the domain's owner.
            assert_eq!(
                created.body["domain"]["owner"].as_str(),
                Some(ACCOUNT),
                "case {case:?}: expected the caller's account as the domain owner, \
                 got {created:?}"
            );

            // Both other operations that answer with a `DomainDescription` report it too.
            let described = describe_domain_in(&router, region, domain).await;
            assert_eq!(
                (described.status, domain_bucket(&described, &case)),
                (200, expected.clone()),
                "case {case:?}: expected DescribeDomain to report {expected:?}, \
                 got {described:?}"
            );
            let deleted = delete_domain_in(&router, region, domain).await;
            assert_eq!(
                (deleted.status, domain_bucket(&deleted, &case)),
                (200, expected.clone()),
                "case {case:?}: expected DeleteDomain to report {expected:?}, got {deleted:?}"
            );
        }

        // Not minted per instance either: a second emulator reports the same ARN.
        let case = format!("{region} on a fresh emulator");
        let fresh = create_domain_in(&emulator(), region, "fresh").await;
        assert_eq!(
            (fresh.status, domain_bucket(&fresh, &case)),
            (200, expected.clone()),
            "case {case:?}: expected {expected:?}, got {fresh:?}"
        );
    }
}

#[tokio::test]
async fn a_published_asset_bucket_account_belongs_to_its_region_alone() {
    for (region, published) in PUBLISHED {
        for other in OTHER_REGIONS {
            assert_ne!(
                other, region,
                "case {other:?}: this row is about a Region other than {region:?}"
            );
            let case = format!("{other} must not report {region}'s asset bucket");

            let created = create_domain_in(&emulator(), other, "elsewhere").await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected CreateDomain 200, got {created:?}"
            );
            let arn = domain_bucket(&created, &case);
            assert_ne!(
                asset_bucket_account(&arn, other, &case),
                published,
                "case {case:?}: {published:?} is the asset-bucket account of {region:?} \
                 alone, got {arn:?}"
            );
        }
    }
}
