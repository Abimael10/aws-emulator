// SPDX-License-Identifier: CC0-1.0

//! The associations between a Direct Connect gateway and the gateways the account
//! holds — the `DirectConnectGatewayAssociation` shape's data, owned by this service
//! rather than rebuilt per response.
//!
//! An association is minted by `CreateDirectConnectGatewayAssociation`, read by
//! `DescribeDirectConnectGatewayAssociations` and torn down by
//! `DeleteDirectConnectGatewayAssociation`, and all three render the stored record
//! through [`GatewayAssociation::output`], so a create followed by a describe answers
//! the same bytes.
//!
//! Everything about the ASSOCIATED gateway (its type, owner, Region and, for a
//! virtual private gateway, the VPC prefixes it advertises) is captured from what the
//! owner of gateways answered at create time — see [`super::ec2_network`].

use super::{ComAmazonawsDirectconnectOvertureserviceState, DueTransition};

/// `DirectConnectGatewayAssociationState`'s `associating`: "The initial state after
/// calling CreateDirectConnectGatewayAssociation."
pub const ASSOCIATION_STATE_ASSOCIATING: &str = "associating";

/// `DirectConnectGatewayAssociationState`'s `associated`: "The Direct Connect gateway
/// and virtual private gateway or transit gateway are successfully associated and
/// ready to pass traffic."
pub const ASSOCIATION_STATE_ASSOCIATED: &str = "associated";

/// `DirectConnectGatewayAssociationState`'s `disassociating`: "The initial state
/// after calling DeleteDirectConnectGatewayAssociation."
pub const ASSOCIATION_STATE_DISASSOCIATING: &str = "disassociating";

/// `DirectConnectGatewayAssociationState`'s `disassociated`: "The virtual private
/// gateway or transit gateway is disassociated from the Direct Connect gateway.
/// Traffic flow between the Direct Connect gateway and virtual private gateway or
/// transit gateway is stopped."
///
/// The TERMINAL state, so it is the one state no stored record is ever in: reaching
/// it is what ends the association, after which `DescribeDirectConnectGatewayAssociations`
/// reports it no longer and its gateway is free to be associated again. It names the
/// move the clock owes a `disassociating` record
/// ([`ComAmazonawsDirectconnectOvertureserviceState::settle_due_associations`]).
pub const ASSOCIATION_STATE_DISASSOCIATED: &str = "disassociated";

/// The most prefixes this emulator stores on one association.
///
/// The emulator's own ceiling, not an AWS quota: `RouteFilterPrefixList` carries no
/// `@length`, so the list a request names is caller-sized and every later render
/// walks it. Bounded here, once, so one request cannot make every later describe of
/// the association cost what that request chose.
pub const MAX_ALLOWED_PREFIXES_PER_ASSOCIATION: usize = 100;

/// A stored association between a Direct Connect gateway and a virtual private
/// gateway or transit gateway.
#[derive(Debug, Clone)]
pub struct GatewayAssociation {
    /// `DirectConnectGatewayAssociation$associationId`.
    pub association_id: String,
    /// The Direct Connect gateway this association is made on.
    pub direct_connect_gateway_id: String,
    /// The account that owns that Direct Connect gateway.
    pub direct_connect_gateway_owner_account: String,
    /// One of the four `ASSOCIATION_STATE_*` members above.
    pub association_state: String,
    /// `DirectConnectGatewayAssociation$stateChangeError`: "the error message if the
    /// state of an object failed to advance". `None` while no advance has failed.
    pub state_change_error: Option<String>,
    /// The associated gateway's identifier, as its owner holds it.
    pub associated_gateway_id: String,
    /// The associated gateway's `GatewayType`.
    pub associated_gateway_type: String,
    /// The account that owns the associated gateway.
    pub associated_gateway_owner_account: String,
    /// The Region the associated gateway is located in.
    pub associated_gateway_region: String,
    /// "The Amazon VPC prefixes to advertise to the Direct Connect gateway" as they
    /// stood when the association was made, as CIDR strings.
    ///
    /// For a transit gateway association these ARE the advertised prefixes: the
    /// request names them. For a virtual private gateway association they are the
    /// blocks of the attached VPCs at create time, which is what is reported when the
    /// owner of those VPCs cannot be asked — the prefixes such an association
    /// advertises are otherwise the attached VPCs' CURRENT blocks, so a block added
    /// to the VPC is advertised too (see [`Self::advertised_prefixes`]).
    pub allowed_prefixes: Vec<String>,
    /// The state change the clock owes this association.
    pub due: Option<DueTransition>,
}

impl GatewayAssociation {
    /// Whether this association's advertised prefixes are its gateway's attached VPC
    /// blocks — true exactly for a virtual private gateway association, whose
    /// prefixes are not the caller's to choose.
    pub fn advertises_its_vpc_blocks(&self) -> bool {
        self.associated_gateway_type == super::ec2_network::VIRTUAL_PRIVATE_GATEWAY
    }

    /// The prefixes this association advertises to its Direct Connect gateway, given
    /// what the owner of VPCs reports for the associated gateway RIGHT NOW (`None`
    /// when it could not be asked, or when the gateway is not one with attached
    /// VPCs).
    ///
    /// A virtual private gateway association advertises the blocks of the VPCs it is
    /// attached to, so one associated with the VPC after the association was made is
    /// advertised as well; the stored set is what is reported when its owner answers
    /// nothing. A transit gateway association advertises the prefixes the request
    /// named and nothing else.
    pub fn advertised_prefixes(&self, attached: Option<&[String]>) -> Vec<String> {
        match attached.filter(|_| self.advertises_its_vpc_blocks()) {
            Some(blocks) => blocks.to_vec(),
            None => self.allowed_prefixes.clone(),
        }
    }

    /// Whether this association still HOLDS its gateway, so no other association may
    /// be made on that gateway ("must not be associated with another Direct Connect
    /// gateway").
    ///
    /// Every STORED association does: traffic stops only once the disassociation has
    /// completed, and completing it is what removes the record.
    pub fn holds_its_gateway(&self) -> bool {
        self.association_state != ASSOCIATION_STATE_DISASSOCIATED
    }

    /// Render this record into the generated `DirectConnectGatewayAssociation` output
    /// shape: the ONE rendering of a stored association, so every operation answers
    /// the same field set. `advertised_prefixes` is what
    /// [`Self::advertised_prefixes`] answered.
    pub fn output(
        &self,
        advertised_prefixes: Vec<String>,
    ) -> crate::ops::types::DirectConnectGatewayAssociation {
        // `DirectConnectGatewayAssociation$virtualGatewayId` and its Region and owner
        // are the virtual-private-gateway spelling of the same three values
        // `associatedGateway` carries, so they are reported only where that gateway IS
        // a virtual private gateway.
        let is_virtual_private = self.advertises_its_vpc_blocks();
        crate::ops::types::DirectConnectGatewayAssociation {
            direct_connect_gateway_id: Some(self.direct_connect_gateway_id.clone()),
            direct_connect_gateway_owner_account: Some(
                self.direct_connect_gateway_owner_account.clone(),
            ),
            association_state: Some(self.association_state.clone()),
            state_change_error: self.state_change_error.clone(),
            associated_gateway: Some(crate::ops::types::AssociatedGateway {
                id: Some(self.associated_gateway_id.clone()),
                r#type: Some(self.associated_gateway_type.clone()),
                owner_account: Some(self.associated_gateway_owner_account.clone()),
                region: Some(self.associated_gateway_region.clone()),
            }),
            association_id: Some(self.association_id.clone()),
            allowed_prefixes_to_direct_connect_gateway: Some(
                advertised_prefixes
                    .into_iter()
                    .map(|cidr| crate::ops::types::RouteFilterPrefix { cidr: Some(cidr) })
                    .collect(),
            ),
            // `associatedCoreNetwork` is "the ID of the Cloud WAN core network
            // associated with the Direct Connect gateway attachment": only an
            // association to a core network carries one, and no operation of this
            // service makes one.
            associated_core_network: None,
            virtual_gateway_id: is_virtual_private.then(|| self.associated_gateway_id.clone()),
            virtual_gateway_region: is_virtual_private
                .then(|| self.associated_gateway_region.clone()),
            virtual_gateway_owner_account: is_virtual_private
                .then(|| self.associated_gateway_owner_account.clone()),
        }
    }
}

impl ComAmazonawsDirectconnectOvertureserviceState {
    /// Mint the next `associationId`.
    ///
    /// AWS hands out a version-4 UUID for a Direct Connect gateway association, the
    /// same opaque form a `directConnectGatewayId` takes, so both come from the one
    /// minter ([`ComAmazonawsDirectconnectOvertureserviceState::next_uuid`]).
    pub fn next_association_id(&mut self) -> String {
        self.next_uuid()
    }

    /// The stored associations in creation order, newest last.
    pub fn associations_in_order(&self) -> Vec<&GatewayAssociation> {
        self.association_order
            .iter()
            .filter_map(|id| self.gateway_associations.get(id))
            .collect()
    }

    /// The association that still HOLDS the gateway `gateway_id`, if any — the one
    /// that stands in the way of associating that gateway with another Direct Connect
    /// gateway ("must not be associated with another Direct Connect gateway").
    pub fn association_holding(&self, gateway_id: &str) -> Option<&GatewayAssociation> {
        self.associations_in_order()
            .into_iter()
            .find(|association| {
                association.associated_gateway_id == gateway_id && association.holds_its_gateway()
            })
    }

    /// Apply every association state change the clock has already made due — the
    /// association half of
    /// [`ComAmazonawsDirectconnectOvertureserviceState::settle_due_transitions`].
    ///
    /// Reaching [`ASSOCIATION_STATE_DISASSOCIATED`] ENDS the association, so that move
    /// removes the record rather than storing the terminal state: the gateway is free
    /// again and no reader reports an association that no longer carries traffic.
    pub fn settle_due_associations(&mut self) {
        let now = ::std::time::Instant::now();
        let mut ended: Vec<String> = Vec::new();
        for (association_id, association) in &mut self.gateway_associations {
            let Some(due) = &association.due else { continue };
            if now < due.at {
                continue;
            }
            if due.to == ASSOCIATION_STATE_DISASSOCIATED {
                ended.push(association_id.clone());
            } else {
                association.association_state = due.to.clone();
                association.due = None;
            }
        }
        if !ended.is_empty() {
            for association_id in ended {
                self.gateway_associations.remove(&association_id);
            }
            self.association_order
                .retain(|id| self.gateway_associations.contains_key(id));
        }
    }
}
