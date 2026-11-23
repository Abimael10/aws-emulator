// SPDX-License-Identifier: CC0-1.0

//! THE HOP: placing the network interfaces a directory's servers answer on, in the service
//! that OWNS network interfaces.
//!
//! `DirectoryVpcSettings$SubnetIds`' own documentation says "Directory Service creates a
//! directory server and a DNS server in each of these subnets", and
//! `DirectoryConnectSettings$SubnetIds` places an AD Connector's servers the same way. A
//! server placed in a subnet of the caller's VPC is ON that network: it occupies an address
//! of the subnet's block and answers on an elastic network interface there, which is what
//! every client that works from the directory's network reads — the interfaces are what hold
//! the addresses `DirectoryDescription$DnsIpAddrs` reports, what the security group this
//! service provisions is applied to, and what blocks a caller's own `DeleteSubnet`/`DeleteVpc`
//! with `DependencyViolation` for as long as the directory is there.
//!
//! A network interface is not a resource of this service: it is Amazon EC2's. So each one is
//! CREATED THERE, over the same in-process callback the subnets of the placement are resolved
//! through and the directory's security group is minted through ([`super::ec2_subnets`], whose
//! client builder this module shares) — an interface this service only recorded would hold no
//! address of the account's network and answer no `DescribeNetworkInterfaces`.
//!
//! What a create places is ONE interface per subnet of the placement EC2 resolved
//! ([`provision_server_interfaces`]): in that subnet, carrying the group provisioned for the
//! directory's servers, named after the directory in its description — so a client that asks
//! the owner of network interfaces which of them belong to a directory is answered. The
//! address each one holds is the OWNER's to assign out of the subnet's range, and the create
//! reads it back off the answer: that address is what the subnet's server answers DNS at
//! ([`super::Directory::server_address_in`]), so the address this service reports and the
//! address the interface really holds are one value, and two directories placed in one subnet
//! are given two different ones. A subnet the owner could not resolve gets no interface,
//! exactly as it contributes no address and no zone.
//!
//! Interfaces placed FOR a directory's servers live exactly as long as they do, so the
//! teardown `DeleteDirectory` starts releases them again ([`release_server_interfaces`]) — in
//! the same owner, over the same hop — BEFORE the group they carry, which EC2 refuses to
//! delete while anything still holds it. Once released, the owner of network interfaces
//! answers for them the way it answers for any identifier it holds nothing under, which is
//! what lets the caller take its own subnets and VPC away again.

use std::collections::BTreeMap;

use super::directory_network::SubnetNetwork;
use super::ec2_subnets::ec2;

/// One network interface this service placed for a directory's server, as Amazon EC2 — the
/// service that owns network interfaces — answered the create that placed it.
///
/// Only what EC2 itself reported is kept: the identifier the teardown releases, and the
/// address the owner ASSIGNED the interface out of its subnet's range, which is the address
/// that subnet's server answers DNS at ([`super::Directory::server_address_in`]). Nothing here
/// is derived from the request or counted by this service.
#[derive(Debug, Clone)]
pub struct ServerInterface {
    /// The interface's own identifier, as EC2 minted it.
    pub interface_id: String,
    /// The private IPv4 address EC2 assigned it, out of the subnet's own range; `None` for an
    /// interface it reports none for, whose server then holds no address of this service's
    /// reporting either.
    pub private_ip: Option<String>,
}

/// The description the network interface of a directory's server carries: a sentence naming
/// the service that created it and the directory whose server answers on it.
///
/// Worded like the description of the group this service provisions beside it
/// ([`super::ec2_security_groups::controllers_group_description`]) and carrying the
/// directory's own identifier, which is what a client reconciling the directory against the
/// owner of network interfaces recognises its interfaces by.
#[must_use]
pub fn server_interface_description(directory_id: &str) -> String {
    format!("AWS created network interface for directory {directory_id}")
}

/// Place the network interfaces the directory `directory_id`'s servers answer on and report
/// the identifier EC2 minted for each one, keyed by the subnet it sits in — empty when there
/// is nothing to place.
///
/// ONE interface per subnet of `subnet_networks`, the subnets the create resolved against the
/// owner of subnets: a subnet the owner reports nothing for holds no server of this account's
/// and gets no interface, so nothing is placed on a network the account does not have. That
/// bounds the hops at the placement bound every create already applies
/// ([`super::MAX_PLACEMENT_SUBNETS`]), whatever the request body carries.
///
/// `security_group_id` is the group this service provisioned for those servers, which each
/// interface is created with so the one group really governs the directory's traffic; an
/// interface created without it gets the VPC's default group, as EC2 does for any interface
/// whose create names no group.
///
/// The ADDRESS each interface holds is the owner's to choose — "if you don't specify an IPv4
/// address, Amazon EC2 selects one for you from the subnet's IPv4 CIDR range"
/// (`CreateNetworkInterfaceRequest$PrivateIpAddress`) — and is read back off its answer, so
/// the address the directory's server answers at is one that subnet really has free: two
/// directories placed in one subnet are given two different addresses, which neither could be
/// if this service counted them itself.
///
/// Called by a create AFTER it has released this service's store, so no guard is held across
/// the hops. A hop that could not be made is evidence of nothing and gates no create: the
/// directory keeps the placement the request named and simply has no interface in that
/// subnet, rather than an identifier naming no interface anywhere.
pub async fn provision_server_interfaces(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    directory_id: &str,
    subnet_networks: &BTreeMap<String, SubnetNetwork>,
    security_group_id: Option<&str>,
) -> BTreeMap<String, ServerInterface> {
    let mut placed: BTreeMap<String, ServerInterface> = BTreeMap::new();
    if subnet_networks.is_empty() {
        return placed;
    }
    let Some(client) = ec2(ctx, region) else {
        ::tracing::warn!(
            "the request names no Region label, so no network interface can be created for \
             the directory's servers"
        );
        return placed;
    };
    let description = server_interface_description(directory_id);
    for subnet_id in subnet_networks.keys() {
        let mut request = client
            .create_network_interface()
            .subnet_id(subnet_id)
            .description(description.as_str());
        if let Some(group_id) = security_group_id {
            request = request.groups(group_id);
        }
        match request.send().await {
            // The identifier EC2 minted and the address it assigned, read off its own answer:
            // this service records the interface the owner of network interfaces holds and
            // never a value of its own.
            Ok(output) => {
                if let Some(interface) = output.network_interface {
                    if let Some(interface_id) = interface.network_interface_id {
                        placed.insert(
                            subnet_id.clone(),
                            ServerInterface {
                                interface_id,
                                private_ip: interface.private_ip_address,
                            },
                        );
                    }
                }
            }
            Err(error) => {
                // At `warn` for the same reason the security group's hop logs one: a
                // directory whose servers have no interface on the caller's network is worth
                // seeing, and this IS why.
                ::tracing::warn!(
                    %region,
                    %subnet_id,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the network interface for a directory's server could not be created in EC2"
                );
            }
        }
    }
    placed
}

/// Release the network interfaces this service placed for a directory's servers: delete them
/// in the service that OWNS network interfaces.
///
/// Each interface exists FOR one of the directory's servers — nothing else of the account was
/// ever given it — so the teardown that takes those servers away takes their interfaces with
/// them: an interface left behind would go on occupying an address of the caller's subnet and
/// go on blocking the caller's own `DeleteSubnet` with `DependencyViolation`. The identifiers
/// are the ones the create recorded off EC2's own answers, so this deletes the interfaces EC2
/// minted and never an identifier of this service's own.
///
/// An interface the owner still holds — it could not be asked, or it refused the delete
/// because something of the account still has the interface attached — is logged rather than
/// reported back: the directory those servers belonged to is gone by now, so there is no
/// record left to name it and nothing for a caller to reconcile against.
///
/// Called with this service's store RELEASED, so no guard is held across the hops, and BEFORE
/// the group those interfaces carry is released: EC2 refuses to delete a security group an
/// interface still holds.
pub async fn release_server_interfaces(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    interface_ids: &[String],
) {
    if interface_ids.is_empty() {
        return;
    }
    let Some(client) = ec2(ctx, region) else {
        ::tracing::warn!(
            "the request names no Region label, so the network interfaces of the directory's \
             servers cannot be released"
        );
        return;
    };
    for interface_id in interface_ids {
        if let Err(error) = client
            .delete_network_interface()
            .network_interface_id(interface_id)
            .send()
            .await
        {
            // At `warn`: a directory torn down with an interface left behind in the
            // caller's subnet is worth seeing, and this IS why.
            ::tracing::warn!(
                %region,
                %interface_id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a network interface of a directory's server could not be deleted in EC2"
            );
        }
    }
}

impl super::Directory {
    /// The network interfaces this service placed for this directory's servers, as the create
    /// recorded them off EC2's own answers: what the teardown releases again, in subnet order.
    #[must_use]
    pub fn provisioned_network_interface_ids(&self) -> Vec<String> {
        self.provisioned_network_interfaces
            .values()
            .map(|interface| interface.interface_id.clone())
            .collect()
    }

    /// The address the owner of network interfaces assigned the interface this directory's
    /// server in the subnet `subnet_id` answers on, or `None` for a subnet no interface was
    /// placed in or one EC2 reported no address for.
    #[must_use]
    pub fn interface_address_in(&self, subnet_id: &str) -> Option<&str> {
        self.provisioned_network_interfaces
            .get(subnet_id)?
            .private_ip
            .as_deref()
    }
}
