// SPDX-License-Identifier: CC0-1.0

//! The AWS Global Infrastructure reference data AWS publishes as public parameters: the
//! `/aws/service/global-infrastructure` hierarchy every account can read in every Region.
//!
//! "You can call the Parameter Store `GetParametersByPath` API operation to view a list of
//! all active Regions" and "to find the long name of a Region, call `GetParameter` on
//! `/aws/service/global-infrastructure/regions/<region>/longName`" (AWS Systems Manager
//! User Guide, "Calling AWS Region, service, and endpoint public parameters") — so this
//! hierarchy is READABLE reference data rather than a namespace an account is kept out of:
//! Region discovery through it is a bootstrap step for a CLI or an installer, not an
//! authorization decision.
//!
//! Nothing here is the account's and nothing writes it: it is AWS's own catalogue, which
//! `PutParameter` cannot write into (the `/aws` prefix is reserved,
//! [`super::RESERVED_NAME_PREFIXES`]), so a read of one of these names reports what AWS
//! publishes under it and a later read reports the same value.
//!
//! What this endpoint holds of that catalogue is its REGION half: one parameter per Region
//! whose value is the Region code, and, under each, the Region's long name, the partition
//! it belongs to and the domain its endpoints are served from ([`REGION_PROPERTIES`]). None
//! of those values is restated here: the long name is read from the catalogue of published
//! Region display data the service that owns Region facts holds
//! (`ComAmazonawsEc2Amazonec2State`'s `region_display`), so the two surfaces that publish a
//! Region's name cannot drift apart, and the partition and the domain FOLLOW from the
//! Region code, the way an ARN's partition does. The service half of the hierarchy
//! (`/aws/service/global-infrastructure/services/…`) is per-service data this emulator
//! holds none of, so it publishes none of it rather than inventing a catalogue of services.

use std::time::SystemTime;

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

/// The path AWS publishes its Global Infrastructure catalogue under, canonical (no leading
/// `/`), as the guide section above spells it.
pub const GLOBAL_INFRASTRUCTURE_PATH: &str = "aws/service/global-infrastructure";

/// The level of that catalogue the Regions are published at: "`GetParametersByPath` …
/// `--path /aws/service/global-infrastructure/regions`" (same section).
pub const REGIONS_PATH: &str = "aws/service/global-infrastructure/regions";

/// The property name whose value is "the long name of a Region", e.g. `US East (N.
/// Virginia)` (same section).
pub const PROPERTY_LONG_NAME: &str = "longName";

/// The property name whose value is the partition a Region belongs to, as the partition
/// field of an ARN spells it ([`PARTITION_AWS`] and the two partitions of their own).
pub const PROPERTY_PARTITION: &str = "partition";

/// The property name whose value is the domain the Region's service endpoints are served
/// from (`amazonaws.com`, and `amazonaws.com.cn` in the China partition).
pub const PROPERTY_DOMAIN: &str = "domain";

/// The partition the commercial Regions belong to.
pub const PARTITION_AWS: &str = "aws";

/// The partition the China Regions belong to, whose Region codes are the ones that start
/// with [`CHINA_REGION_PREFIX`].
pub const PARTITION_CHINA: &str = "aws-cn";

/// The partition the AWS GovCloud (US) Regions belong to, whose Region codes are the ones
/// that start with [`GOV_CLOUD_REGION_PREFIX`].
pub const PARTITION_GOV_CLOUD: &str = "aws-us-gov";

/// The Region-code prefix of the China partition.
pub const CHINA_REGION_PREFIX: &str = "cn-";

/// The Region-code prefix of the AWS GovCloud (US) partition.
pub const GOV_CLOUD_REGION_PREFIX: &str = "us-gov-";

/// The endpoint domain of the commercial and GovCloud partitions.
pub const DOMAIN_AWS: &str = "amazonaws.com";

/// The endpoint domain of the China partition.
pub const DOMAIN_CHINA: &str = "amazonaws.com.cn";

/// The properties published UNDER each Region, in the order this catalogue publishes them.
///
/// The names are the ones the guide section above names for a Region; the value of each is
/// derived from the Region itself ([`published_property`]), so no property is stated twice.
pub const REGION_PROPERTIES: &[&str] = &[PROPERTY_DOMAIN, PROPERTY_LONG_NAME, PROPERTY_PARTITION];

/// The Regions AWS publishes in its Global Infrastructure catalogue, in Region-code order:
/// the commercial Regions, and those of the China and AWS GovCloud (US) partitions, which
/// that catalogue publishes beside them.
///
/// The codes alone: everything published about a Region is derived from the code and from
/// the published display data of that Region ([`published_property`]), so this is a list of
/// which Regions are published and not a second copy of what is published about them. Every
/// code here is one that display catalogue holds, which the suite covering this catalogue
/// checks Region by Region.
pub const PUBLISHED_REGIONS: &[&str] = &[
    "af-south-1",
    "ap-east-1",
    "ap-east-2",
    "ap-northeast-1",
    "ap-northeast-2",
    "ap-northeast-3",
    "ap-south-1",
    "ap-south-2",
    "ap-southeast-1",
    "ap-southeast-2",
    "ap-southeast-3",
    "ap-southeast-4",
    "ap-southeast-5",
    "ap-southeast-6",
    "ap-southeast-7",
    "ca-central-1",
    "ca-west-1",
    "cn-north-1",
    "cn-northwest-1",
    "eu-central-1",
    "eu-central-2",
    "eu-north-1",
    "eu-south-1",
    "eu-south-2",
    "eu-west-1",
    "eu-west-2",
    "eu-west-3",
    "il-central-1",
    "me-central-1",
    "me-south-1",
    "mx-central-1",
    "sa-east-1",
    "us-east-1",
    "us-east-2",
    "us-gov-east-1",
    "us-gov-west-1",
    "us-west-1",
    "us-west-2",
];

/// The partition the Region `code` belongs to, from the code itself: an ARN's partition is
/// `aws-cn` for a China Region and `aws-us-gov` for an AWS GovCloud (US) one, and `aws`
/// everywhere else.
pub fn partition_of(code: &str) -> &'static str {
    if code.starts_with(CHINA_REGION_PREFIX) {
        PARTITION_CHINA
    } else if code.starts_with(GOV_CLOUD_REGION_PREFIX) {
        PARTITION_GOV_CLOUD
    } else {
        PARTITION_AWS
    }
}

/// The domain the service endpoints of the Region `code` are served from, which follows
/// from its partition: the China partition serves its endpoints from its own domain and
/// every other partition from AWS's.
pub fn domain_of(code: &str) -> &'static str {
    if partition_of(code) == PARTITION_CHINA {
        DOMAIN_CHINA
    } else {
        DOMAIN_AWS
    }
}

/// The value published for the property `property` of the Region `code`, or `None` when
/// the catalogue publishes no such property of it ([`REGION_PROPERTIES`]) — or, for the long
/// name, when the published display data holds none for that Region.
pub fn published_property(code: &str, property: &str) -> Option<String> {
    match property {
        // The one property that is a NAME rather than a consequence of the code: read from
        // the catalogue of published Region display data, so the long name this parameter
        // reports and the one a Region's own facts report are one value.
        PROPERTY_LONG_NAME => ec2::region_display(code).map(|display| display.long_name.to_string()),
        PROPERTY_PARTITION => Some(partition_of(code).to_string()),
        PROPERTY_DOMAIN => Some(domain_of(code).to_string()),
        _ => None,
    }
}

/// When the catalogue was last published, which is what a read reports as the parameter's
/// `LastModifiedDate`.
///
/// Reference data, not a resource: nothing at this endpoint holds the instant AWS last
/// published it, so the instant reported is the epoch rather than one that would move
/// between two reads of the same name.
pub const PUBLISHED_AT: SystemTime = SystemTime::UNIX_EPOCH;

/// Every name this catalogue publishes, with the value published under it: the Region
/// itself (whose value is the Region code) and each property of it
/// ([`REGION_PROPERTIES`]), with the leading `/` a parameter name is addressed by.
pub fn published_names() -> Vec<(String, String)> {
    let mut names = Vec::with_capacity(PUBLISHED_REGIONS.len() * (1 + REGION_PROPERTIES.len()));
    for code in PUBLISHED_REGIONS {
        names.push((format!("/{REGIONS_PATH}/{code}"), (*code).to_string()));
        for property in REGION_PROPERTIES {
            let Some(value) = published_property(code, property) else {
                continue;
            };
            names.push((format!("/{REGIONS_PATH}/{code}/{property}"), value));
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Region of the catalogue publishes every property the catalogue says it does —
    /// which includes the long name, so a Region published here is one the display
    /// catalogue holds — and the Region's own parameter publishes its code.
    #[test]
    fn every_region_publishes_every_property() {
        let names = published_names();
        for code in PUBLISHED_REGIONS {
            let own = format!("/{REGIONS_PATH}/{code}");
            assert!(
                names
                    .iter()
                    .any(|(name, value)| name == &own && value == code),
                "case {code:?}: expected the Region's own parameter to publish its code, got {:?}",
                names.iter().find(|(name, _)| name == &own),
            );
            for property in REGION_PROPERTIES {
                let name = format!("/{REGIONS_PATH}/{code}/{property}");
                assert!(
                    names.iter().any(|(held, _)| *held == name),
                    "case {code:?}: expected {name} to be published, got none",
                );
            }
        }
    }

    /// A Region's partition and endpoint domain follow from its code, for each of the three
    /// partitions the catalogue publishes Regions of.
    #[test]
    fn the_partition_and_the_domain_follow_from_the_region_code() {
        let rows = [
            ("us-east-1", PARTITION_AWS, DOMAIN_AWS),
            ("eu-west-1", PARTITION_AWS, DOMAIN_AWS),
            ("cn-north-1", PARTITION_CHINA, DOMAIN_CHINA),
            ("cn-northwest-1", PARTITION_CHINA, DOMAIN_CHINA),
            ("us-gov-west-1", PARTITION_GOV_CLOUD, DOMAIN_AWS),
            ("us-gov-east-1", PARTITION_GOV_CLOUD, DOMAIN_AWS),
        ];
        for (code, partition, domain) in rows {
            assert_eq!(
                partition_of(code),
                partition,
                "case {code:?}: expected the partition {partition}, got {:?}",
                partition_of(code),
            );
            assert_eq!(
                domain_of(code),
                domain,
                "case {code:?}: expected the domain {domain}, got {:?}",
                domain_of(code),
            );
        }
    }

    /// No name is published twice, since a parameter name addresses one parameter.
    #[test]
    fn no_name_is_published_twice() {
        let mut names: Vec<String> = published_names().into_iter().map(|(name, _)| name).collect();
        let held = names.len();
        names.sort();
        names.dedup();
        assert_eq!(
            names.len(),
            held,
            "expected every published name to be distinct, got {} of {held}",
            names.len(),
        );
    }
}
