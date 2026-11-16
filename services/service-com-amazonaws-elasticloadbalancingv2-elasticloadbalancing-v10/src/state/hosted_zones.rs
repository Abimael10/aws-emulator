// SPDX-License-Identifier: CC0-1.0

//! The Route 53 hosted zone Elastic Load Balancing publishes per Region and load balancer
//! family, which a reported load balancer's `CanonicalHostedZoneId` names.

use super::*;

/// One Region's published Route 53 hosted zones: the Region, the zone an Application Load
/// Balancer of it is aliased through, and the zone a Network Load Balancer of it is
/// aliased through.
pub(super) struct HostedZoneEntry {
    pub(super) region: &'static str,
    pub(super) application: &'static str,
    pub(super) network: &'static str,
}

/// The Route 53 hosted zone ids Elastic Load Balancing publishes per Region and load
/// balancer family, which `LoadBalancer$CanonicalHostedZoneId` reports ("the ID of the
/// Amazon Route 53 hosted zone associated with the load balancer").
///
/// These are the service's own published identifiers — the "Elastic Load Balancing
/// endpoints and quotas" table of hosted zone ids, which a client needs to alias a record
/// at the load balancer — not anything derived from a request or minted per run: the same
/// Region and family always answer the same id, for every account and every load balancer.
/// Application Load Balancers share their Region's zone with Classic load balancers, and
/// Network Load Balancers have their own.
///
/// A Region this table does not carry reports no hosted zone rather than a made-up one: an
/// id of the right shape that resolves to nothing would be worse than an absent member.
///
/// The `application` column is the SAME published column as the Classic endpoint's, which
/// `services/service-com-amazonaws-elasticloadbalancing-elasticloadbalancing-v7`'s
/// `CLASSIC_ELB_HOSTED_ZONE_IDS` carries for its own model: the two tables must agree on
/// every Region both cover, and they do. They cannot be ONE table in a shared crate
/// without a dependency cycle — that crate already depends on this one, for the shared
/// endpoint's API-version dispatch — so this is the cross-reference that keeps them in
/// step, and a Region added to either belongs in both.
pub(super) const HOSTED_ZONES: &[HostedZoneEntry] = &[
    HostedZoneEntry {
        region: "af-south-1",
        application: "Z268VQBMOI5EKX",
        network: "Z203XCE67M25HM",
    },
    HostedZoneEntry {
        region: "ap-east-1",
        application: "Z3DQVH9N71FHZ0",
        network: "Z12Y7K3UBGUAD1",
    },
    HostedZoneEntry {
        region: "me-south-1",
        application: "ZS929ML54UICD",
        network: "Z3QSRYVP46NYYV",
    },
    HostedZoneEntry {
        region: "us-east-1",
        application: "Z35SXDOTRQ7X7K",
        network: "Z26RNL4JYFTOTI",
    },
    HostedZoneEntry {
        region: "us-east-2",
        application: "Z3AADJGX6KTTL2",
        network: "ZLMOA37VPKANP",
    },
    HostedZoneEntry {
        region: "us-west-1",
        application: "Z368ELLRRE2KJ0",
        network: "Z24FKFUX50B4VV",
    },
    HostedZoneEntry {
        region: "us-west-2",
        application: "Z1H1FL5HABSF5",
        network: "Z18D5FSROUN65G",
    },
    HostedZoneEntry {
        region: "ca-central-1",
        application: "ZQSVJUPU6J1EY",
        network: "Z2EPGBW3API2WT",
    },
    HostedZoneEntry {
        region: "eu-west-1",
        application: "Z32O12XQLNTSW2",
        network: "Z2IFOLAFXWLO4F",
    },
    HostedZoneEntry {
        region: "eu-west-2",
        application: "ZHURV8PSTC4K8",
        network: "ZD4D7Y8KGAS4G",
    },
    HostedZoneEntry {
        region: "eu-west-3",
        application: "Z3Q77PNBQS71R4",
        network: "Z1CMS0P5QUZ6D5",
    },
    HostedZoneEntry {
        region: "eu-central-1",
        application: "Z215JYRZR1TBD5",
        network: "Z3F0SRJ5LGBH90",
    },
    HostedZoneEntry {
        region: "eu-north-1",
        application: "Z23TAZ6LKFMNIO",
        network: "Z1UDT6IFJ4EJM",
    },
    HostedZoneEntry {
        region: "eu-south-1",
        application: "Z3ULH7SSC9OV64",
        network: "Z23146JA1KNAFP",
    },
    HostedZoneEntry {
        region: "ap-south-1",
        application: "ZP97RAFLXTNZK",
        network: "ZVDDRBQ08TROA",
    },
    HostedZoneEntry {
        region: "ap-northeast-1",
        application: "Z14GRHDCWA56QT",
        network: "Z31USIVHYNEOWT",
    },
    HostedZoneEntry {
        region: "ap-northeast-2",
        application: "ZWKZPGTI48KDX",
        network: "ZIBE1TIR4HY56",
    },
    HostedZoneEntry {
        region: "ap-northeast-3",
        application: "Z5LXEXXYQ4HTP",
        network: "Z1GWIQ4HH19I5X",
    },
    HostedZoneEntry {
        region: "ap-southeast-1",
        application: "Z1LMS91P8CMLE5",
        network: "ZKVM4W9LS7TM",
    },
    HostedZoneEntry {
        region: "ap-southeast-2",
        application: "Z1GM3OXH4ZPM65",
        network: "ZCT6FZBF4DROD",
    },
    HostedZoneEntry {
        region: "sa-east-1",
        application: "Z2P70J7HTTTPLU",
        network: "ZTK26PT1VY4CU",
    },
];

/// The published hosted zone of a load balancer of `family` in `region`, or `None` when
/// the service publishes none for it — a Region [`HOSTED_ZONES`] does not carry, or a
/// Gateway Load Balancer, which is reached through its own VPC endpoints rather than
/// through a DNS name a hosted zone resolves.
pub fn canonical_hosted_zone_id(region: &str, family: LoadBalancerFamily) -> Option<&'static str> {
    let entry = HOSTED_ZONES.iter().find(|entry| entry.region == region)?;
    match family {
        LoadBalancerFamily::Application => Some(entry.application),
        LoadBalancerFamily::Network => Some(entry.network),
        LoadBalancerFamily::Gateway => None,
    }
}
