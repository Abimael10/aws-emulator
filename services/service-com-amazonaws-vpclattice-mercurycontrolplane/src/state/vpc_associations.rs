// SPDX-License-Identifier: CC0-1.0

//! The service-network/VPC association: what one holds, the status the clock owes it, and
//! how the store answers questions about the VPCs a service network has.
//!
//! An association links one service network to one VPC, so "it enables all the resources
//! within that VPC to be clients and communicate with other services in the service
//! network" (`CreateServiceNetworkVpcAssociation`'s own documentation). It is a RESOURCE of
//! its own — it has an id, an ARN, a creator and a status — so it lives in the one store
//! beside the service associations rather than as a list on the service network, and every
//! operation that reports one reads it from here.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::ops::types::DnsOptions;

use super::ComAmazonawsVpclatticeMercurycontrolplaneState;

/// An association between one service network and one VPC.
///
/// `ServiceNetworkVpcAssociationStatus` is non-terminal right after a create
/// (`CREATE_IN_PROGRESS`), so the clock owes this record its `ACTIVE` status the way it
/// owes a service association one: the due instant is stored and compared on read
/// ([`Self::status`]), and nothing sleeps for it.
#[derive(Debug, Clone)]
pub struct ServiceNetworkVpcAssociation {
    pub id: String,
    pub arn: String,
    pub created_by: String,
    pub created_at: SystemTime,
    pub last_updated_at: SystemTime,
    /// The instant the clock owes this association its `ACTIVE` status.
    pub active_at: SystemTime,
    pub service_network_id: String,
    pub service_network_arn: String,
    pub service_network_name: String,
    /// The `vpcIdentifier` the create associated, which the model constrains to a bare VPC
    /// id (`^vpc-(([0-9a-z]{8})|([0-9a-z]{17}))$`) — never an ARN — and which every read
    /// reports as `vpcId`.
    pub vpc_id: String,
    /// The security groups the create asked for, or `None` when it asked for none:
    /// "Security groups aren't added by default" (the member's own documentation), so an
    /// association without them reports no list rather than an empty one.
    pub security_group_ids: Option<Vec<String>>,
    /// `privateDnsEnabled` exactly as the create set it, and absent when the create said
    /// nothing about it: the model documents no default for the member, so nothing is
    /// invented for it here.
    pub private_dns_enabled: Option<bool>,
    /// The `dnsOptions` the create sent, stored as the modelled structure so every read
    /// reports the same options.
    pub dns_options: Option<DnsOptions>,
    pub tags: HashMap<String, String>,
    /// Creation order, so a listing pages deterministically. See
    /// [`super::encode_cursor`].
    pub sequence: u64,
}

impl ServiceNetworkVpcAssociation {
    /// `ServiceNetworkVpcAssociationStatus` at `now`: the transient create status until the
    /// stored due instant passes, `ACTIVE` from then on. A pure function of stored state
    /// and the clock, so reading it changes nothing a later caller sees.
    pub fn status(&self, now: SystemTime) -> &'static str {
        if now >= self.active_at {
            "ACTIVE"
        } else {
            "CREATE_IN_PROGRESS"
        }
    }
}

impl ComAmazonawsVpclatticeMercurycontrolplaneState {
    /// The association of this exact (service network, VPC) pair, if there is one.
    ///
    /// "You can't use this operation if there is a disassociation in progress"
    /// (`CreateServiceNetworkVpcAssociation`'s documentation), and a VPC is associated with
    /// a service network at most once, so a create naming a pair this answers is a
    /// `ConflictException`.
    pub fn vpc_association_of(
        &self,
        service_network_id: &str,
        vpc_id: &str,
    ) -> Option<&ServiceNetworkVpcAssociation> {
        self.vpc_associations.values().find(|association| {
            association.service_network_id == service_network_id && association.vpc_id == vpc_id
        })
    }

    /// How many VPCs are associated with `service_network_id`, counted from the association
    /// records at read time rather than from a stored counter — the same way the service
    /// associations are counted.
    pub fn associated_vpc_count(&self, service_network_id: &str) -> i64 {
        i64::try_from(
            self.vpc_associations
                .values()
                .filter(|association| association.service_network_id == service_network_id)
                .count(),
        )
        .unwrap_or(i64::MAX)
    }
}
