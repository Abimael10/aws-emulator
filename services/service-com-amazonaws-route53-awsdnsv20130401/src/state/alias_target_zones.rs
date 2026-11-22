// SPDX-License-Identifier: CC0-1.0

//! Which hosted zone an `AliasTarget` names, and the problem a target that does
//! not lie within that zone is refused with.
//!
//! An alias "[routes] traffic to the resource" its target names, and the target
//! names that resource by the hosted zone it lives in (`HostedZoneId`) plus its
//! own name inside that zone (`DNSName`), so a target name outside the named
//! zone names no resource there and the alias routes nowhere. That is the one
//! rule this module decides, for every zone whose CONTENTS are decidable here:
//!
//! * a zone AWS PUBLISHES the id of, whose domain is published with it —
//!   `AliasTarget$HostedZoneId` states one id for every CloudFront distribution
//!   and one for every Global Accelerator accelerator, and one per Region for
//!   the Amazon S3 website endpoints ("Specify the hosted zone ID for the region
//!   that you created the bucket in", with the published table beside it);
//! * a hosted zone of the ACCOUNT's own, which this store holds the name of —
//!   "Another Route 53 resource record set in your hosted zone: Specify the
//!   hosted zone ID of your hosted zone. (An alias resource record set can't
//!   reference a resource record set in a different hosted zone.)", whose
//!   `DNSName` is then "the value of the `Name` element for a resource record
//!   set in the current hosted zone".
//!
//! A zone id read off the RESOURCE instead — a load balancer's
//! `CanonicalHostedZoneId`, an API's `regionalHostedZoneId`, an interface
//! endpoint's `HostedZoneId` — fixes no domain and names no zone of this
//! account, so there is nothing to hold a target against and every target it
//! names is accepted.
//!
//! Read from `smithy_query(query="doc",shape="AliasTarget",member="HostedZoneId")`
//! and the same query for `DNSName`.

use crate::state::{dns_name_address, name_is_in_zone, normalize_zone_id};

/// The target zones AWS publishes ONE id for, each with the domain the names of
/// the resources it holds lie in.
///
/// `AliasTarget$HostedZoneId` spells both out — "CloudFront distribution:
/// Specify `Z2FDTNDATAQYW2`" and "Global Accelerator accelerator: Specify
/// `Z2BJ6XQ5FK7U4H`" — and `AliasTarget$DNSName` the domain each one's names lie
/// in: "the name of the associated CloudFront distribution, such as
/// `da1b2c3d4e5.cloudfront.net`", and an accelerator's own DNS name, which is a
/// label under `awsglobalaccelerator.com`.
pub const GLOBAL_TARGET_ZONE_DOMAINS: &[(&str, &str)] = &[
    ("Z2FDTNDATAQYW2", "cloudfront.net"),
    ("Z2BJ6XQ5FK7U4H", "awsglobalaccelerator.com"),
];

/// The hosted zone id AWS publishes for each Region's Amazon S3 WEBSITE
/// endpoints, paired with the Region whose endpoints that zone holds.
///
/// `AliasTarget$HostedZoneId` points at this table by name for a bucket
/// configured as a static website ("Specify the hosted zone ID for the region
/// that you created the bucket in. For more information about valid values, see
/// the table Amazon S3 Website Endpoints in the Amazon Web Services General
/// Reference"), and `AliasTarget$DNSName` at the endpoint whose domain goes with
/// it ("the domain name of the Amazon S3 website endpoint that you created the
/// bucket in, for example, `s3-website.us-east-2.amazonaws.com`"). One id per
/// Region is what makes a target checkable from the request alone: the id fixes
/// the Region, and the Region fixes the endpoint domain
/// ([`s3_website_domains`]).
///
/// SOURCE: the published *Amazon S3 Website Endpoints* table of the Amazon Web
/// Services General Reference, which that member's own documentation links.
pub const S3_WEBSITE_TARGET_ZONE_REGIONS: &[(&str, &str)] = &[
    ("Z11KHD8FBVPUYU", "af-south-1"),
    ("ZNB98KWMFR0R6", "ap-east-1"),
    ("Z2M4EHUR26P7ZW", "ap-northeast-1"),
    ("Z3W03O7B5YMIYP", "ap-northeast-2"),
    ("Z2YQB5RD63NC85", "ap-northeast-3"),
    ("Z11RGJOFQNVJUP", "ap-south-1"),
    ("Z3O0J2DXBE1FTB", "ap-southeast-1"),
    ("Z1WCIGYICN2BYD", "ap-southeast-2"),
    ("Z1QDHH18159H29", "ca-central-1"),
    ("Z5CN8UMXT92WN", "cn-north-1"),
    ("Z282HJ1KT0DH03", "cn-northwest-1"),
    ("Z21DNDUVLTQW6Q", "eu-central-1"),
    ("Z3BAZG2TWCNX0D", "eu-north-1"),
    ("Z30OZKI7KPW7MI", "eu-south-1"),
    ("Z1BKCTXD74EZPE", "eu-west-1"),
    ("Z3GKZC51ZF0DB4", "eu-west-2"),
    ("Z3R1K369G5AVDG", "eu-west-3"),
    ("Z1MPMWCPA7YB62", "me-south-1"),
    ("Z7KQH4QJS55SO", "sa-east-1"),
    ("Z3AQBSTGFYJSTF", "us-east-1"),
    ("Z2O1EMRO9K5GLX", "us-east-2"),
    ("Z2NIFVYYW2VKV1", "us-gov-east-1"),
    ("Z31GFT0UA1I2HV", "us-gov-west-1"),
    ("Z2F56UZL2M1ACD", "us-west-1"),
    ("Z3BJ6K6RIION7M", "us-west-2"),
];

/// The label an Amazon S3 website endpoint host carries, in both spellings.
const S3_WEBSITE_LABEL: &str = "s3-website";

/// The DNS suffix AWS publishes a Region's endpoints under: the China Regions
/// publish theirs under their own suffix, every other Region under
/// `amazonaws.com`.
fn endpoint_suffix(region: &str) -> &'static str {
    match region.starts_with("cn-") {
        true => "amazonaws.com.cn",
        false => "amazonaws.com",
    }
}

/// The domains a Region's Amazon S3 website endpoints lie in.
///
/// Amazon S3 publishes that surface under BOTH spellings of the endpoint label
/// — `s3-website-<Region>.<suffix>` and `s3-website.<Region>.<suffix>` (Amazon
/// S3 User Guide, *Website endpoints*; `AliasTarget$DNSName` quotes the dotted
/// one) — and both name the same endpoints of the same Region, so a target
/// written either way lies within that Region's website zone.
#[must_use]
pub fn s3_website_domains(region: &str) -> [String; 2] {
    let suffix = endpoint_suffix(region);
    [
        format!("{S3_WEBSITE_LABEL}-{region}.{suffix}"),
        format!("{S3_WEBSITE_LABEL}.{region}.{suffix}"),
    ]
}

/// The domains the zone `zone_id` holds the names of its resources in, or an
/// empty list for an id AWS publishes no zone for — which there is nothing to
/// hold a target against.
///
/// The id is compared without regard to case, like every other hosted zone id
/// this service resolves.
#[must_use]
pub fn published_target_zone_domains(zone_id: &str) -> Vec<String> {
    if let Some((_, domain)) = GLOBAL_TARGET_ZONE_DOMAINS
        .iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(zone_id))
    {
        return vec![(*domain).to_string()];
    }
    S3_WEBSITE_TARGET_ZONE_REGIONS
        .iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(zone_id))
        .map(|(_, region)| s3_website_domains(region).to_vec())
        .unwrap_or_default()
}

/// The problem a change that writes such an alias is refused with.
///
/// REQUEST-DERIVED: the sentence names the target, the record type and the zone
/// the change asked for, and nothing else.
fn target_not_in_zone(target: &str, rr_type: &str, zone_id: &str) -> String {
    format!(
        "Tried to create an alias that targets {target}, type {rr_type} in zone {zone_id}, but \
         the alias target name does not lie within the target zone"
    )
}

/// Why the alias target `(target, zone_id)` does not lie within the hosted zone
/// it names, or `None` when it does — and `None` as well for an id that names no
/// zone whose contents are known here, which there is nothing to hold a target
/// against (see this module's own documentation).
///
/// `zone_in_store` is the NAME of the hosted zone this store holds under
/// `zone_id`, when it holds one: such a zone holds exactly the names inside it,
/// so that is the domain its targets are held against.
///
/// A resource's endpoint in THIS emulator is published as the address a client
/// dials rather than as a name in the owning service's zone (the `host:port` the
/// service that opened the data plane reported), so a target that names an
/// address ([`dns_name_address`]) names an endpoint of that zone and lies within
/// it.
#[must_use]
pub fn alias_target_zone_problem(
    target: &str,
    rr_type: &str,
    zone_id: &str,
    zone_in_store: Option<&str>,
) -> Option<String> {
    let zone_id = normalize_zone_id(zone_id);
    if dns_name_address(target).is_some() {
        return None;
    }
    let domains = match zone_in_store {
        Some(zone_name) => vec![zone_name.to_string()],
        None => published_target_zone_domains(zone_id),
    };
    // An id that names no zone whose contents are known here — one read off the
    // resource — fixes no domain, so there is nothing to hold the target
    // against and the alias is accepted whatever it names.
    if domains.is_empty() {
        return None;
    }
    (!domains
        .iter()
        .any(|domain| name_is_in_zone(target, domain)))
    .then(|| target_not_in_zone(target, rr_type, zone_id))
}
