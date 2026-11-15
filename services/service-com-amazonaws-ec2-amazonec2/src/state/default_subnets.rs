// SPDX-License-Identifier: CC0-1.0

//! The default subnet of ONE Availability Zone: which block of its default VPC it takes, how
//! the one a zone already has is found, and the refusals a request for one is answered with.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every
//! other primitive there.

use super::*;

impl ComAmazonawsEc2Amazonec2State {
    /// The default subnet the VPC `vpc_id` holds in the Availability Zone `zone_name`, by id,
    /// or `None` when that zone holds none.
    ///
    /// "You can have only one default subnet per Availability Zone"
    /// (`CreateDefaultSubnet`), so this is the ONE question a request for one asks, and it is
    /// asked of the subnet store itself — a subnet of that VPC, in that zone, carrying
    /// `DefaultForAz` — rather than of a second index beside it, so a default subnet that was
    /// DELETED stops answering it the moment the subnet is gone.
    ///
    /// A READ: a zone with no default subnet is not given one here.
    pub fn default_subnet_in_zone(&self, vpc_id: &str, zone_name: &str) -> Option<String> {
        self.subnets
            .iter()
            .find(|(_, subnet)| {
                subnet.vpc_id.as_deref() == Some(vpc_id)
                    && subnet.availability_zone.as_deref() == Some(zone_name)
                    && subnet.default_for_az == Some(true)
            })
            .map(|(subnet_id, _)| subnet_id.clone())
    }
}

/// The IPv4 block the default subnet of the `zone_index`-th Availability Zone of a Region
/// takes out of `vpc_cidr_block`, or `None` when that block has no such child block.
///
/// "When we create a default VPC … we add a size /20 IPv4 CIDR block" to each zone's subnet
/// (Amazon VPC User Guide, "Default VPCs"), so a zone's default subnet is the n-th
/// [`DEFAULT_SUBNET_PREFIX_LENGTH`] block of its VPC's own block, counted in the Region's own
/// zone order ([`availability_zones_in_region`]).
///
/// The ONE definition of which block belongs to which zone, so the whole network's writer
/// ([`ComAmazonawsEc2Amazonec2State::insert_default_subnets`]) and a request for the default
/// subnet of a single zone cannot disagree about it: a zone whose default subnet was deleted
/// gets exactly the block it had back.
pub fn default_subnet_cidr_block(vpc_cidr_block: &str, zone_index: u32) -> Option<String> {
    nth_subnet_cidr_block(vpc_cidr_block, DEFAULT_SUBNET_PREFIX_LENGTH, zone_index)
}

/// EC2's error code for a request naming an Availability Zone a subnet cannot be created in —
/// one the Region does not have.
pub const INVALID_AVAILABILITY_ZONE: &str = "InvalidAvailabilityZone";

/// The sentence EC2 refuses a subnet-creating request that names a zone the Region does not
/// have with: it quotes the value the request sent, names the member it sent it under, and
/// lists the zones that Region offers for that member.
///
/// ONE wording site for the class, shared by every operation that creates a subnet in a
/// caller-named zone — `CreateSubnet` and `CreateDefaultSubnet` answer it under different
/// error codes, which is a fact of each operation, but the sentence is the same one. `known`
/// is the Region's zone names or its zone ids, picked by the caller to match the member it is
/// refusing, so this never has to read `member` to decide what to list.
pub fn subnet_zones_sentence(member: &str, requested: &str, known: &[String]) -> String {
    format!(
        "Value ({requested}) for parameter {member} is invalid. Subnets can currently only \
         be created in the following availability zones: {known}.",
        known = known.join(", "),
    )
}

/// Refuse a request for the default subnet of a zone the Region does not have: HTTP 400
/// [`INVALID_AVAILABILITY_ZONE`], worded by [`subnet_zones_sentence`].
pub fn invalid_availability_zone<E>(
    member: &str,
    requested: &str,
    known: &[String],
) -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_AVAILABILITY_ZONE,
        400,
        subnet_zones_sentence(member, requested, known),
    )
}

/// EC2's error code for a request for a default subnet in an Availability Zone that already
/// has one.
pub const DEFAULT_SUBNET_ALREADY_EXISTS: &str = "DefaultSubnetAlreadyExistsInAvailabilityZone";

/// Refuse a second default subnet in one Availability Zone: HTTP 400
/// [`DEFAULT_SUBNET_ALREADY_EXISTS`].
///
/// "You can have only one default subnet per Availability Zone" (`CreateDefaultSubnet`), so
/// the second request for one is refused rather than served another subnet. The sentence names
/// the zone the request asked about — the one value the caller has to change — and not the
/// subnet already there, which is state of the account the caller did not ask about.
pub fn default_subnet_already_exists<E>(zone_name: &str) -> Scaffolded<E> {
    Scaffolded::service_error(
        DEFAULT_SUBNET_ALREADY_EXISTS,
        400,
        format!("A default subnet already exists in availability zone {zone_name}."),
    )
}

/// EC2's error code for a request that needs the account's default VPC in a Region where it
/// has none.
pub const DEFAULT_VPC_DOES_NOT_EXIST: &str = "DefaultVpcDoesNotExist";

/// Refuse a request for a default subnet in a Region whose default VPC is gone: HTTP 400
/// [`DEFAULT_VPC_DOES_NOT_EXIST`].
///
/// A default subnet is created "in your default VPC" (`CreateDefaultSubnet`), so a caller that
/// deleted that VPC has nowhere to put one until it asks for a new one (`CreateDefaultVpc`).
/// The sentence has no variable part: it says only that the account has no default VPC where
/// it asked.
pub fn default_vpc_does_not_exist<E>() -> Scaffolded<E> {
    Scaffolded::service_error(
        DEFAULT_VPC_DOES_NOT_EXIST,
        400,
        "No default VPC for this user.",
    )
}
