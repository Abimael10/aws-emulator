// SPDX-License-Identifier: CC0-1.0

//! The Route 53 hosted zones an alias record for one of this service's endpoints targets.
//!
//! A module of its own under [`crate::state`], because this is one concern several members
//! share — `DomainNameConfiguration$HostedZoneId` and
//! `EndpointConfigurationResponse$PortalDomainHostedZoneId` — and AWS ASSIGNS such an id
//! per endpoint and publishes no formula for it, so the ids it publishes are carried here
//! verbatim and the derivation below is only the fallback for an endpoint this service
//! carries no published id for. [`crate::state`] re-exports every public item here, so a
//! handler reaches them the way it reaches the rest of this service's state helpers.

/// How many characters follow the leading `Z` of a hosted zone id, matching the
/// fourteen-character ids AWS publishes for its endpoint hosted zones.
pub(super) const HOSTED_ZONE_ID_LEN: usize = 13;

/// A hosted zone id DERIVED from the parts that decide WHICH endpoint it belongs to — the
/// fallback for an endpoint whose published id this service does not carry.
///
/// AWS assigns ONE opaque hosted zone id per service endpoint per Region and publishes no
/// formula for it, so an id it publishes is CARRIED verbatim where this service has it
/// ([`CLOUDFRONT_HOSTED_ZONE_ID`], [`REGIONAL_ENDPOINT_HOSTED_ZONES`],
/// [`PORTAL_DOMAIN_HOSTED_ZONES`]) and derived from those
/// parts otherwise, rather than invented afresh per read. The mixing is
/// [`::service_common::stable_hex_id`] — the tree's one derived-id mixer, reproducible
/// across builds — uppercased and cut to the published LENGTH: a leading `Z` and
/// [`HOSTED_ZONE_ID_LEN`] characters, which the mixer renders as uppercase hexadecimal
/// (a subset of the uppercase alphanumerics AWS's own ids use, e.g. `Z1UJRXOUMOOFQ8`).
/// The same parts therefore always answer the same id, and two different part lists
/// answer different ones.
pub(super) fn derived_hosted_zone_id(parts: &[&str]) -> String {
    let mixed: String = ::service_common::stable_hex_id(parts)
        .to_uppercase()
        .chars()
        .take(HOSTED_ZONE_ID_LEN)
        .collect();
    format!("Z{mixed}")
}

/// One Region's hosted zone for the domains API Gateway generates and manages for a
/// developer portal: the zone, and the Region it is the zone OF.
#[derive(Debug)]
pub struct PortalDomainHostedZone {
    pub region: &'static str,
    pub hosted_zone_id: &'static str,
}

/// The hosted zones AWS PUBLISHES for portal default domains, one entry per Region.
///
/// Every portal in a Region is served on the one domain API Gateway generates and manages
/// there (`EndpointConfigurationResponse$PortalDefaultDomainName`), so the zone an alias
/// record for it targets is a fact about the REGION: AWS answers the same id for every
/// portal created in that Region and for every read of it, as the recorded AWS answers for
/// `us-west-2` show. AWS ASSIGNS the id and publishes no formula for it — the same as the
/// one it publishes for a CloudFront distribution ([`CLOUDFRONT_HOSTED_ZONE_ID`]) — so the
/// ids it publishes are CARRIED here rather than computed, and a Region this table does not
/// name falls back to [`derived_hosted_zone_id`] (see
/// [`portal_domain_hosted_zone_id`]).
///
/// `tests/all/portal_hosted_zone.rs` keeps its OWN copy of these ids and asserts the two
/// agree Region for Region, so an entry here always has a published AWS answer behind it
/// and a Region added to either belongs in both.
pub const PORTAL_DOMAIN_HOSTED_ZONES: &[PortalDomainHostedZone] = &[PortalDomainHostedZone {
    region: "us-west-2",
    hosted_zone_id: "Z091913938PZVWK6ZMXI7",
}];

/// The published hosted zone of the portal default domain in `region`, or `None` for a
/// Region [`PORTAL_DOMAIN_HOSTED_ZONES`] carries no published id for.
pub fn published_portal_domain_hosted_zone_id(region: &str) -> Option<&'static str> {
    PORTAL_DOMAIN_HOSTED_ZONES
        .iter()
        .find(|zone| zone.region == region)
        .map(|zone| zone.hosted_zone_id)
}

/// The hosted zone id of the portal domain
/// (`EndpointConfigurationResponse$PortalDomainHostedZoneId`), the zone an alias record
/// for the portal's domain points at.
///
/// The member is REQUIRED on the modelled response, so a portal always reports one, and it
/// is one id per Region: the id AWS publishes for the Region
/// ([`published_portal_domain_hosted_zone_id`]) wherever this service carries it, since a
/// caller writes a Route 53 alias record against that zone and a zone id of this emulator's
/// own names no zone their account can reach. A Region with no published id carried here
/// still reports one id of its own, derived by [`derived_hosted_zone_id`], so the member
/// keeps its per-Region invariance and its stability everywhere.
pub fn portal_domain_hosted_zone_id(region: &str) -> String {
    published_portal_domain_hosted_zone_id(region)
        .map_or_else(|| derived_hosted_zone_id(&[region]), str::to_string)
}

/// The `EndpointType` whose domain is fronted by a CloudFront distribution rather than by
/// a regional endpoint: "an AWS-managed certificate that will be used by the
/// edge-optimized endpoint for this domain name" (`DomainNameConfiguration$CertificateArn`
/// and the rest of that shape's edge-optimized members).
pub const EDGE_ENDPOINT_TYPE: &str = "EDGE";

/// The one hosted zone every CloudFront distribution is aliased through, whatever Region
/// the resource was created in. It is PUBLISHED rather than derived: Route 53's own
/// `AliasTarget$HostedZoneId` states it ("CloudFront distribution: Specify
/// `Z2FDTNDATAQYW2`"), and
/// `services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice` reports the
/// same value for its own CloudFront-fronted custom domains.
pub const CLOUDFRONT_HOSTED_ZONE_ID: &str = "Z2FDTNDATAQYW2";

/// One Region's hosted zone for API Gateway's REGIONAL endpoint: the zone, and the Region
/// it is the zone OF.
#[derive(Debug)]
pub struct RegionalEndpointHostedZone {
    pub region: &'static str,
    pub hosted_zone_id: &'static str,
}

/// The hosted zones AWS PUBLISHES for API Gateway's regional endpoints, one entry per
/// Region.
///
/// Every regional custom domain name in a Region is served on that Region's API Gateway
/// endpoint, so the zone an ALIAS record for it targets is a fact about the REGION: AWS
/// answers the same id for every domain name created there and for every read of it. The
/// ids are AWS's own — it assigns them and publishes no formula — so they are CARRIED here
/// verbatim, from the "Amazon API Gateway" rows of the hosted-zone table AWS publishes for
/// Route 53 alias targets (the same table [`CLOUDFRONT_HOSTED_ZONE_ID`] comes from). A
/// Region this table does not name falls back to [`derived_hosted_zone_id`] (see
/// [`endpoint_hosted_zone_id`]).
///
/// `tests/all/custom_domain_hosted_zone.rs` keeps its OWN copy of these ids and asserts the
/// two agree Region for Region, so an entry here always has a published AWS answer behind
/// it and a Region added to either belongs in both.
pub const REGIONAL_ENDPOINT_HOSTED_ZONES: &[RegionalEndpointHostedZone] = &[
    RegionalEndpointHostedZone { region: "af-south-1", hosted_zone_id: "Z2DHW2332DAMTN" },
    RegionalEndpointHostedZone { region: "ap-east-1", hosted_zone_id: "Z3FD1VL90ND7K5" },
    RegionalEndpointHostedZone { region: "ap-northeast-1", hosted_zone_id: "Z1YSHQZHG15GKL" },
    RegionalEndpointHostedZone { region: "ap-northeast-2", hosted_zone_id: "Z20JF4UZKIW1U8" },
    RegionalEndpointHostedZone { region: "ap-northeast-3", hosted_zone_id: "Z2YQB5RD63NC85" },
    RegionalEndpointHostedZone { region: "ap-south-1", hosted_zone_id: "Z3VO1THU9YC4UR" },
    RegionalEndpointHostedZone { region: "ap-southeast-1", hosted_zone_id: "ZL327KTPIQFUL" },
    RegionalEndpointHostedZone { region: "ap-southeast-2", hosted_zone_id: "Z2RPCDW04V8134" },
    RegionalEndpointHostedZone { region: "ca-central-1", hosted_zone_id: "Z19DQILCV0OWEC" },
    RegionalEndpointHostedZone { region: "eu-central-1", hosted_zone_id: "Z1U9ULNL0V5AJ3" },
    RegionalEndpointHostedZone { region: "eu-north-1", hosted_zone_id: "Z3UWIKFBOOGXPP" },
    RegionalEndpointHostedZone { region: "eu-west-1", hosted_zone_id: "ZLY8HYME6SFDD" },
    RegionalEndpointHostedZone { region: "eu-west-2", hosted_zone_id: "ZJ5UAJN8Y3Z2Q" },
    RegionalEndpointHostedZone { region: "eu-west-3", hosted_zone_id: "Z3KY65QIEKYHQQ" },
    RegionalEndpointHostedZone { region: "me-south-1", hosted_zone_id: "Z20ZBPC0SS8806" },
    RegionalEndpointHostedZone { region: "sa-east-1", hosted_zone_id: "ZCMLWB8V5SYIT" },
    RegionalEndpointHostedZone { region: "us-east-1", hosted_zone_id: "Z1UJRXOUMOOFQ8" },
    RegionalEndpointHostedZone { region: "us-east-2", hosted_zone_id: "ZOJJZC49E0EPZ" },
    RegionalEndpointHostedZone { region: "us-west-1", hosted_zone_id: "Z2MUQ32089INYE" },
    RegionalEndpointHostedZone { region: "us-west-2", hosted_zone_id: "Z2OJLYMUO9EFXC" },
];

/// The published hosted zone of API Gateway's regional endpoint in `region`, or `None` for
/// a Region [`REGIONAL_ENDPOINT_HOSTED_ZONES`] carries no published id for.
pub fn published_regional_endpoint_hosted_zone_id(region: &str) -> Option<&'static str> {
    REGIONAL_ENDPOINT_HOSTED_ZONES
        .iter()
        .find(|zone| zone.region == region)
        .map(|zone| zone.hosted_zone_id)
}

/// "The Amazon Route 53 Hosted Zone ID of the endpoint"
/// (`DomainNameConfiguration$HostedZoneId`) — the zone an ALIAS record for the custom
/// domain names as its target, which is a fact about the endpoint rather than about the
/// resource: one value per endpoint type, and for a regional endpoint one per Region.
///
/// * an `EDGE` endpoint is fronted by CloudFront, so every one of them is aliased through
///   the single published [`CLOUDFRONT_HOSTED_ZONE_ID`], in every Region;
/// * a `REGIONAL` endpoint lives in the Region the domain name was created in, so its zone
///   is the one AWS publishes for that Region's API Gateway endpoint
///   ([`published_regional_endpoint_hosted_zone_id`]) — a caller writes a Route 53 alias
///   record against that zone, and a zone id of this emulator's own names no zone their
///   account can reach — and never the CloudFront one. A Region with no published id
///   carried here still reports one id of its own ([`derived_hosted_zone_id`] over the
///   endpoint type and the Region), so the member keeps its per-Region invariance
///   everywhere.
///
/// Derived on every read from the stored endpoint type and the stored Region, so a domain
/// name reports the same id for the life of the endpoint and reports a different one once
/// an update moves it onto another endpoint type.
pub fn endpoint_hosted_zone_id(endpoint_type: &str, region: &str) -> String {
    if endpoint_type == EDGE_ENDPOINT_TYPE {
        return CLOUDFRONT_HOSTED_ZONE_ID.to_string();
    }
    published_regional_endpoint_hosted_zone_id(region)
        .map_or_else(|| derived_hosted_zone_id(&[endpoint_type, region]), str::to_string)
}
