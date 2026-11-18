// SPDX-License-Identifier: CC0-1.0

//! Where a configuration `Arn` says its configuration lives, which is part of
//! resolving it.
//!
//! An ARN names a resource in ONE Region, and a regional endpoint serves only its
//! own Region's resources — so the Region segment of a caller-supplied ARN is part
//! of the identity of the resource the request addresses. Reading it is what keeps
//! a configuration ARN carried across Regions from resolving to whatever this
//! endpoint happens to hold at the same name, which for `UpdateConfiguration`
//! would land a write on the wrong resource.
//!
//! Two different answers come out of that reading, and they are different gates:
//! a Region segment naming another Region AWS publishes ([`is_published_region`])
//! is a well-formed ARN of a resource this endpoint does not serve, so the
//! configuration is NOT FOUND here ([`crate::state::RESOURCE_NOT_FOUND_MESSAGE`],
//! 404) — distinct from the bad request the endpoint answers for a reference of
//! its OWN Region that resolves to nothing
//! ([`crate::state::CONFIGURATION_ARN_NOT_FOUND_MESSAGE`]); a Region segment that
//! is no Region at all is an input the caller can only have mistyped, which is the
//! earlier, generic parameter refusal ([`crate::state::BAD_REQUEST_MESSAGE`], 400).
//!
//! The decision is taken from the REQUEST alone — the ARN as submitted and the
//! Region the endpoint serves — so it lives here, beside the state the resolution
//! then reads, and every operation that names a configuration by `Arn` takes it
//! through [`region_refusal`] before its own lookup. One reading for the family,
//! so the readers, the revision listing, the update and the delete cannot disagree
//! about which Region's configurations this endpoint holds.

/// The input member the configuration operations carry their target's ARN in, as
/// the API names it on the wire: those operations bind their `Arn` label as `arn`,
/// which is what a refusal reports in `invalidParameter`.
pub const CONFIGURATION_ARN_MEMBER: &str = "arn";

/// The resource type an MSK configuration ARN names, which is the resource field
/// of `arn:<partition>:kafka:<region>:<account>:configuration/<name>/<id>`.
pub const CONFIGURATION_RESOURCE_TYPE: &str = "configuration";

/// The ARN namespace this service's own resources live in.
const KAFKA_VENDOR: &str = "kafka";

/// Every Amazon Web Services Region code, which is what a Region segment is
/// matched against to tell "another Region" from "no Region at all".
///
/// The tree's ONE published-Region catalog
/// (`emulator_telemetry::policy::KNOWN_REGIONS`, "every AWS region the wire may
/// carry, sorted"), re-exported rather than copied so this reading cannot drift
/// from the list every other service of this emulator reads — the same way the
/// Cognito and Account services take it.
pub const AWS_REGIONS: &[&str] = ::emulator_telemetry::policy::KNOWN_REGIONS;

/// Whether `value` is a Region AWS publishes, compared case-insensitively as a
/// Region name read off a credential scope is.
pub fn is_published_region(value: &str) -> bool {
    AWS_REGIONS
        .iter()
        .any(|region| region.eq_ignore_ascii_case(value))
}

/// What the REGION segment of a configuration `Arn` says about where the named
/// configuration can live, as an endpoint serving `endpoint_region` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArnRegion {
    /// This endpoint's own Region — or a value this reading says nothing about,
    /// which the operation's own lookup then answers.
    Endpoint,
    /// Another Region AWS publishes: a well-formed ARN of a resource this endpoint
    /// does not serve.
    Elsewhere,
    /// A Region segment that is no Region at all.
    Unreadable,
}

/// The Region segment of `arn` when `arn` is shaped like an MSK CONFIGURATION ARN,
/// and `None` for anything else.
///
/// Read through the tree's one caller-supplied-ARN parse
/// ([`::service_common::arn::parse_of`]), which answers `Option` and borrows the
/// fields rather than splitting the value again here. `None` covers a value that
/// is not an ARN, an ARN of another vendor and an ARN naming another kind of this
/// service's resources: none of them is a configuration reference whose Region
/// could be scoped, so each is left to the operation's own lookup, which answers
/// the reference it cannot resolve.
fn configuration_arn_region(arn: &str) -> Option<&str> {
    let parsed = ::service_common::arn::parse_of(arn, KAFKA_VENDOR)?;
    (parsed.resource_type() == CONFIGURATION_RESOURCE_TYPE).then_some(parsed.region)
}

/// Which Region the configuration `arn` names belongs to, relative to the endpoint
/// serving `endpoint_region`.
pub fn arn_region(arn: &str, endpoint_region: &str) -> ArnRegion {
    let Some(named) = configuration_arn_region(arn) else {
        return ArnRegion::Endpoint;
    };
    if named.eq_ignore_ascii_case(endpoint_region) {
        return ArnRegion::Endpoint;
    }
    if is_published_region(named) {
        return ArnRegion::Elsewhere;
    }
    ArnRegion::Unreadable
}

/// The refusal the Region segment of `arn` calls for at an endpoint serving
/// `endpoint_region`, and `None` when it calls for none and the operation's own
/// lookup decides.
///
/// Built HERE rather than in each handler so the whole configuration family
/// answers one condition one way: a configuration of another Region is the
/// service's fixed not-found sentence, and a Region segment that is no Region is
/// the generic parameter refusal naming [`CONFIGURATION_ARN_MEMBER`]. Each handler
/// lifts the payload into its OWN modelled error, because the error channel is the
/// operation's.
pub fn region_refusal(arn: &str, endpoint_region: &str) -> Option<ArnRegionRefusal> {
    match arn_region(arn, endpoint_region) {
        ArnRegion::Endpoint => None,
        ArnRegion::Elsewhere => Some(ArnRegionRefusal::NotFound(
            crate::ops::types::NotFoundException {
                invalid_parameter: Some(CONFIGURATION_ARN_MEMBER.to_string()),
                message: Some(crate::state::RESOURCE_NOT_FOUND_MESSAGE.to_string()),
            },
        )),
        ArnRegion::Unreadable => Some(ArnRegionRefusal::BadRequest(crate::state::malformed_arn(
            CONFIGURATION_ARN_MEMBER,
        ))),
    }
}

/// One of the two refusals [`region_refusal`] reaches, as the payload the calling
/// operation returns through its own modelled error.
#[derive(Debug)]
pub enum ArnRegionRefusal {
    /// HTTP 404: the configuration belongs to another Region, so this endpoint does
    /// not hold it.
    NotFound(crate::ops::types::NotFoundException),
    /// HTTP 400: the Region segment is no Region at all.
    BadRequest(crate::ops::types::BadRequestException),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the Region segment decides between three answers — this endpoint's
    /// Region resolves, another published Region is elsewhere, and anything that is
    /// no Region at all is unreadable — and only a value shaped like a
    /// configuration ARN is read at all.
    #[test]
    fn the_region_segment_places_a_configuration_arn() {
        let configuration = |region: &str| {
            format!(
                "arn:aws:kafka:{region}:000000000000:configuration/a-configuration/\
                 11111111-2222-4333-8444-555555555555-1"
            )
        };
        // The endpoint's own Region (also in the case a credential scope may spell
        // it), other published Regions including other partitions' and ones no
        // issue cited, and Region-shaped text AWS publishes no Region for.
        let cases: &[(&str, ArnRegion)] = &[
            ("us-west-2", ArnRegion::Endpoint),
            ("US-WEST-2", ArnRegion::Endpoint),
            ("us-east-2", ArnRegion::Elsewhere),
            ("ap-southeast-4", ArnRegion::Elsewhere),
            ("us-gov-west-1", ArnRegion::Elsewhere),
            ("xx-fake-1", ArnRegion::Unreadable),
            ("us-west-9", ArnRegion::Unreadable),
            ("not-a-region", ArnRegion::Unreadable),
        ];
        for (region, expected) in cases {
            let found = arn_region(&configuration(region), "us-west-2");
            assert_eq!(
                found, *expected,
                "case {region:?}: expected {expected:?}, got {found:?}"
            );
        }

        // Values that are no configuration ARN carry no Region to scope, so they
        // are left to the operation's own lookup.
        for value in [
            "not-an-arn",
            "arn:aws:kafka",
            "arn:aws:ec2:xx-fake-1:000000000000:instance/i-0a1b2c3d",
            "arn:aws:kafka:xx-fake-1:000000000000:cluster/a-cluster/abc-1",
        ] {
            let found = arn_region(value, "us-west-2");
            assert_eq!(
                found,
                ArnRegion::Endpoint,
                "case {value:?}: a value that names no configuration must be left to the \
                 lookup, got {found:?}"
            );
        }
    }
}
