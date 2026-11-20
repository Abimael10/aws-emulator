// SPDX-License-Identifier: CC0-1.0

//! The Route 53 hosted zone a Region's database endpoints live in, for the control planes
//! that publish those endpoints.
//!
//! `DBCluster$HostedZoneId` and `Endpoint$HostedZoneId` are "the ID that Amazon Route 53
//! assigns when you create a hosted zone": there is ONE zone per Region AND per the DNS
//! domain the control plane that owns the engine publishes its endpoints under, so every
//! cluster and every instance a Region serves under one domain reports the same id. A client
//! writes that id into a Route 53 alias or DNS-failover record, so where this emulator can
//! cite the id AWS allocated the pair it reports THAT id, and mints one only for a pair it
//! cites none for.
//!
//! It lives here, once, because TWO emulated front doors report the member for the same
//! engines — the Query front door the Amazon RDS, Amazon DocumentDB and Amazon Neptune
//! models share, and the graph control plane's own door — and the member's rule is one zone
//! per Region and domain however a caller reached the control plane. One table and one mint
//! mean a published row cited for one door is reported by both, and neither can mint an id
//! the other would not.

/// The DNS domains the control planes behind the shared front door publish their endpoints
/// under, per engine family.
///
/// One statement of each, in the module both front doors read their hosted zones out of: the
/// table below is keyed by the domain, so a second copy of one of these strings that drifted
/// would silently stop matching the published row for a Region and report a minted stand-in
/// instead of AWS's own id.
pub const RDS_ENDPOINT_DOMAIN: &str = "rds.amazonaws.com";
/// The DNS domain the Amazon DocumentDB control plane's endpoints sit in.
pub const DOCDB_ENDPOINT_DOMAIN: &str = "docdb.amazonaws.com";
/// The DNS domain the graph (Amazon Neptune) control plane's endpoints sit in.
pub const NEPTUNE_ENDPOINT_DOMAIN: &str = "neptune.amazonaws.com";

/// The hosted zone ids this emulator can CITE, as `(Region, endpoint domain, hosted zone
/// id)`.
///
/// From the "Amazon Relational Database Service (Amazon RDS)" and "Amazon Neptune" rows of
/// the hosted zone ID table in the Amazon Route 53 Developer Guide ("Values that you specify
/// when you create alias resource record sets"). One row per Region AND domain, because the
/// control plane that owns the engine publishes its own zone for the Region. Extend it as
/// published ids are cited.
const PUBLISHED: &[(&str, &str, &str)] = &[
    ("us-east-1", RDS_ENDPOINT_DOMAIN, "Z2R2ITUGPM61AM"),
    ("us-west-2", RDS_ENDPOINT_DOMAIN, "Z1PVIF0B656C1W"),
    ("us-west-2", NEPTUNE_ENDPOINT_DOMAIN, "Z22EZDAJT6W257"),
];

/// How many characters follow the `Z` of a minted id: the width this emulator mints, which
/// is the width of the ids cited above.
const MINTED_WIDTH: usize = 13;

/// The id AWS published for the endpoints of `region` under `endpoint_domain`, when this
/// emulator cites one.
pub fn published_hosted_zone_id(region: &str, endpoint_domain: &str) -> Option<&'static str> {
    PUBLISHED
        .iter()
        .find(|(published_region, domain, _)| {
            *published_region == region && *domain == endpoint_domain
        })
        .map(|(_, _, published)| *published)
}

/// The hosted zone id reported for the endpoints of `region` under `endpoint_domain`.
///
/// The published id wherever one is cited ([`published_hosted_zone_id`]); otherwise one
/// MINTED from the pair, in the published `Z`-prefixed form. A client comparing a minted one
/// against AWS's own id for that pair will see a different label; what it gets is an id with
/// the member's form, its per-Region-and-domain invariance and its stability across reads —
/// which is what a client resolving an endpoint through the member needs. Reporting nothing
/// instead would say the endpoint sits in no hosted zone at all, which is true of no
/// endpoint these control planes report.
///
/// The mint is a function of the pair and of nothing else — the pair's digest, drawn in the
/// alphabet AWS writes a server-assigned id in ([`crate::minted_base36`]) — so it is stable
/// across reads, across records and across processes, and the two front doors that serve one
/// engine answer one id for a Region.
pub fn hosted_zone_id(region: &str, endpoint_domain: &str) -> String {
    if let Some(published) = published_hosted_zone_id(region, endpoint_domain) {
        return published.to_string();
    }
    format!(
        "Z{}",
        crate::minted_base36(&format!("hosted-zone/{endpoint_domain}/{region}"), MINTED_WIDTH)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row this module cites is reported for its own pair, and for that pair only.
    #[test]
    fn a_cited_pair_reports_its_published_id() {
        for (region, domain, published) in PUBLISHED {
            let case = format!("{region} under {domain}");
            assert_eq!(
                published_hosted_zone_id(region, domain),
                Some(*published),
                "case {case:?}: expected the published id {published:?}, got {:?}",
                published_hosted_zone_id(region, domain)
            );
            assert_eq!(
                hosted_zone_id(region, domain),
                *published,
                "case {case:?}: the reported id is the published one, got {:?}",
                hosted_zone_id(region, domain)
            );
            // The SAME Region under another control plane's domain is another zone.
            let other_domain = if *domain == RDS_ENDPOINT_DOMAIN {
                DOCDB_ENDPOINT_DOMAIN
            } else {
                RDS_ENDPOINT_DOMAIN
            };
            assert_ne!(
                hosted_zone_id(region, domain),
                hosted_zone_id(region, other_domain),
                "case {case:?}: one zone per Region AND domain, so {other_domain} cannot \
                 report the zone of {domain}"
            );
        }
    }

    /// A pair no row cites is MINTED, in the form the member carries, from the pair alone.
    #[test]
    fn an_uncited_pair_is_minted_in_the_published_form() {
        let rows = [
            ("eu-central-1", NEPTUNE_ENDPOINT_DOMAIN),
            ("ap-southeast-2", RDS_ENDPOINT_DOMAIN),
            ("us-east-1", DOCDB_ENDPOINT_DOMAIN),
        ];
        for (region, domain) in rows {
            let case = format!("{region} under {domain}");
            assert_eq!(
                published_hosted_zone_id(region, domain),
                None,
                "case {case:?}: this pair is cited nowhere, so no published id may be found"
            );
            let minted = hosted_zone_id(region, domain);
            assert_eq!(
                minted.len(),
                MINTED_WIDTH + 1,
                "case {case:?}: a minted id is `Z` plus {MINTED_WIDTH} characters, got \
                 {minted:?}"
            );
            assert!(
                minted.starts_with('Z')
                    && minted[1..].chars().all(|character| {
                        character.is_ascii_digit() || character.is_ascii_uppercase()
                    }),
                "case {case:?}: expected a Z-prefixed id in the published alphabet, got \
                 {minted:?}"
            );
            assert_eq!(
                minted,
                hosted_zone_id(region, domain),
                "case {case:?}: the mint is a function of the pair, so a second read reports \
                 the same id"
            );
            assert_ne!(
                minted,
                hosted_zone_id("eu-west-1", domain),
                "case {case:?}: two Regions under one domain are two zones"
            );
        }
    }
}
