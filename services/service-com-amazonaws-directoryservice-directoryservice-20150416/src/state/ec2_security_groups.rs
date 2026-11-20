// SPDX-License-Identifier: CC0-1.0

//! THE HOP: minting the security group a directory's servers sit behind, in the service that
//! OWNS security groups.
//!
//! A directory this service places in a VPC does not sit on that network bare: Directory
//! Service provisions a security group for the servers it runs there and reports it back —
//! `DirectoryVpcSettingsDescription$SecurityGroupId` is "the domain controller security
//! group identifier for the directory" and `DirectoryConnectSettingsDescription$SecurityGroupId`
//! is "the security group identifier for the AD Connector directory". A client that has to
//! authorize traffic to the directory's servers (a trust between two directories in one VPC,
//! any reconciliation of the directory against EC2) works from that id, so the member is part
//! of the placement this service reports, not an optional extra.
//!
//! A security group is not a resource of this service: it is Amazon EC2's. So the group is
//! CREATED THERE, over the same in-process callback the subnets a directory is placed in are
//! resolved through ([`super::ec2_subnets`], whose client builder this module shares) — the
//! id this service then reports names a group the owner of security groups really holds and
//! answers `DescribeSecurityGroups` for. Nothing is minted here: an id this service invented
//! would name a group nothing could authorize a rule against.
//!
//! The group goes in the VPC the directory's servers really sit in, which is the VPC of the
//! subnets the create resolved ([`placement_vpc_id`]) rather than the `VpcId` the request
//! spelled: the subnets are what the servers are placed in, and EC2 is what says which
//! network they belong to. A create whose subnets the owner could not resolve therefore
//! provisions no group and the directory reports none, exactly as it reports no address for
//! a subnet the account does not have.
//!
//! Provisioning the group is not minting it alone: a group with no inbound rule admits
//! nothing, so the servers behind it would be reachable by nothing and the id this service
//! reports would say so. The rules the directory's servers are reached through are authorized
//! on it in the same owner ([`provision_controllers_group`]), each scoped to the block of the
//! VPC those servers sit on, so a client reading the group off the directory finds the
//! baseline it reasons about, adds to, diffs or revokes.
//!
//! A group provisioned FOR a directory's servers lives exactly as long as they do, so the
//! teardown `DeleteDirectory` starts releases it again ([`release_controllers_group`]) — in
//! the same owner, over the same hop — and the directory stops naming a group EC2 no longer
//! holds.

use std::collections::BTreeMap;

use super::directory_network::SubnetNetwork;
use super::ec2_subnets::ec2;

/// The suffix the name of a directory's AWS-provisioned security group carries after the
/// directory's own identifier.
///
/// Directory Service names the group it creates for a directory's servers
/// `<DirectoryId>_controllers` (the Directory Service Administration Guide's "what gets
/// created" for a directory placed in a VPC), so the group is recognisable from the
/// directory it belongs to and two directories in one VPC never collide on it — which is
/// what EC2 requires of a group name.
const CONTROLLERS_GROUP_SUFFIX: &str = "_controllers";

/// The name of the security group this service provisions for the directory `directory_id`.
#[must_use]
pub fn controllers_group_name(directory_id: &str) -> String {
    format!("{directory_id}{CONTROLLERS_GROUP_SUFFIX}")
}

/// One INBOUND rule of the set this service authorizes on the group it provisions for a
/// directory's servers: a protocol, and the ports that protocol's traffic arrives on.
#[derive(Debug, Clone, Copy)]
struct ControllerIngressRule {
    /// The protocol the rule admits, spelled as `IpPermission$IpProtocol` spells one.
    protocol: &'static str,
    /// The first port of the range the rule admits — for ICMP the type rather than a port
    /// ("if the protocol is ICMP, this is the ICMP type", `IpPermission$FromPort`).
    from_port: i32,
    /// The last port of that range — for ICMP the code ("if the protocol is ICMP, this is the
    /// ICMP code", `IpPermission$ToPort`).
    to_port: i32,
}

/// The `tcp` value of `IpPermission$IpProtocol`.
const IP_PROTOCOL_TCP: &str = "tcp";

/// The `udp` value of `IpPermission$IpProtocol`.
const IP_PROTOCOL_UDP: &str = "udp";

/// The `icmp` value of `IpPermission$IpProtocol`.
const IP_PROTOCOL_ICMP: &str = "icmp";

/// Every ICMP type and every ICMP code: "the ICMP type or -1 (all ICMP types)"
/// (`IpPermission$FromPort`) with "-1 (all ICMP codes)" (`IpPermission$ToPort`).
const ALL_ICMP: i32 = -1;

/// The first port of the range the RPC endpoint mapper hands a client into, as the model's
/// rule list writes it ("Range: 1024-65535").
const FIRST_DYNAMIC_PORT: i32 = 1024;

/// The last port of that range, which is the last port there is.
const LAST_DYNAMIC_PORT: i32 = 65535;

/// The first port the global catalog answers on, as the model's rule list writes that rule
/// ("Range: 3268-33269", whose upper end is the published 3269 — the catalog's two ports are
/// 3268 and its TLS port 3269, and no port above them is part of this rule).
const FIRST_GLOBAL_CATALOG_PORT: i32 = 3268;

/// The last port of that range: the global catalog's TLS port.
const LAST_GLOBAL_CATALOG_PORT: i32 = 3269;

/// The inbound rules a directory's servers are reachable through, which is the set this
/// service authorizes on the group it provisions for them.
///
/// Source: the MODEL. `AddIpRoutesRequest$UpdateSecurityGroupForDirectoryControllers` writes
/// out the rules of "the security group that has the description: 'Amazon Web Services created
/// security group for <directory ID> directory controllers'" — one entry per protocol and port
/// range, each sourced from the directory VPC's IPv4 CIDR, with all traffic outbound — and that
/// is the group this service provisions
/// (`smithy_query(query="doc",shape="AddIpRoutesRequest",member="UpdateSecurityGroupForDirectoryControllers")`).
/// This table is that list, in the order the model writes it; nothing else is admitted, so a
/// port absent from the model's list is absent here — deliberately NARROWER than the set some
/// Directory Service prose lists for this group (the NetBIOS name and session ports, the
/// directory's web services port): the model is this emulator's contract, and the rules it
/// writes out are the rules authored.
const CONTROLLER_INGRESS_RULES: &[ControllerIngressRule] = &[
    // The UDP half of the model's list: Kerberos, time, the NetBIOS datagram service, LDAP,
    // the Kerberos change/set-password service and SMB.
    ControllerIngressRule {
        protocol: IP_PROTOCOL_UDP,
        from_port: 88,
        to_port: 88,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_UDP,
        from_port: 123,
        to_port: 123,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_UDP,
        from_port: 138,
        to_port: 138,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_UDP,
        from_port: 389,
        to_port: 389,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_UDP,
        from_port: 464,
        to_port: 464,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_UDP,
        from_port: 445,
        to_port: 445,
    },
    // The TCP half: Kerberos, the RPC endpoint mapper, SMB, the change/set-password service,
    // LDAPS, the range the endpoint mapper hands a client into, and the global catalog.
    ControllerIngressRule {
        protocol: IP_PROTOCOL_TCP,
        from_port: 88,
        to_port: 88,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_TCP,
        from_port: 135,
        to_port: 135,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_TCP,
        from_port: 445,
        to_port: 445,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_TCP,
        from_port: 464,
        to_port: 464,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_TCP,
        from_port: 636,
        to_port: 636,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_TCP,
        from_port: FIRST_DYNAMIC_PORT,
        to_port: LAST_DYNAMIC_PORT,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_TCP,
        from_port: FIRST_GLOBAL_CATALOG_PORT,
        to_port: LAST_GLOBAL_CATALOG_PORT,
    },
    // DNS, which the directory's servers are their clients' name servers over both protocols,
    // and LDAP itself.
    ControllerIngressRule {
        protocol: IP_PROTOCOL_UDP,
        from_port: 53,
        to_port: 53,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_TCP,
        from_port: 53,
        to_port: 53,
    },
    ControllerIngressRule {
        protocol: IP_PROTOCOL_TCP,
        from_port: 389,
        to_port: 389,
    },
    // "Type: All ICMP, Protocol: All, Range: N/A": every ICMP type and every ICMP code, which
    // is how a client reaches a server at all.
    ControllerIngressRule {
        protocol: IP_PROTOCOL_ICMP,
        from_port: ALL_ICMP,
        to_port: ALL_ICMP,
    },
];

/// The description that group carries: a sentence naming the service that created it and the
/// directory whose servers it is for.
#[must_use]
pub fn controllers_group_description(directory_id: &str) -> String {
    format!("AWS created security group for {directory_id} directory controllers")
}

/// Release the security group this service provisioned for a directory's servers: delete it
/// in the service that OWNS security groups, and report whether that owner no longer holds
/// it.
///
/// The group exists FOR the directory's servers — nothing else of the account was ever given
/// it — so the teardown that takes those servers away takes the group with them: a group left
/// behind would keep occupying the account's per-VPC group quota and keep answering
/// `DescribeSecurityGroups` for a directory that no longer exists. `group_id` is the id the
/// create recorded off EC2's own answer, so this deletes the group EC2 minted and never an id
/// of this service's own.
///
/// `false` when the owner still holds it — it could not be asked, or it refused the delete
/// because something of the account still references the group — and the directory then goes
/// on naming the group that is still there, because a report this service withdrew while the
/// group remained would be the same defect in the other direction.
///
/// Called with this service's store RELEASED, so no guard is held across the hop. ONE call,
/// with no caller-sized value in it.
pub async fn release_controllers_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    group_id: &str,
) -> bool {
    let Some(client) = ec2(ctx, region) else {
        ::tracing::warn!(
            "the request names no Region label, so the security group of the directory's \
             servers cannot be released"
        );
        return false;
    };
    match client
        .delete_security_group()
        .group_id(group_id)
        .send()
        .await
    {
        Ok(_) => true,
        Err(error) => {
            // At `warn` for the same reason the create's hop logs one: a directory torn down
            // with its security group left behind in EC2 is worth seeing, and this IS why.
            ::tracing::warn!(
                %region,
                %group_id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the security group of a directory's servers could not be deleted in EC2"
            );
            false
        }
    }
}

/// The VPC a directory placed in `subnet_networks` really sits in: the VPC the subnets the
/// create resolved belong to, as EC2 reports it.
///
/// `None` when no subnet of the placement resolved, or when the owner of subnets reports no
/// VPC for any of them — there is then no network to provision a group in, and nothing is
/// guessed from the request. The subnets of one placement are all in one VPC (a subnet
/// belongs to exactly one), so the first one that resolved answers for the placement.
#[must_use]
pub fn placement_vpc_id(subnet_networks: &BTreeMap<String, SubnetNetwork>) -> Option<String> {
    subnet_networks
        .values()
        .find_map(|network| network.vpc_id.clone())
}

/// Provision the security group the directory `directory_id`'s servers sit behind and report
/// the id EC2 minted for it, or `None` when there is no group to report.
///
/// `vpc_id` is the network the group goes in — [`placement_vpc_id`] of the subnets the create
/// resolved — so a create that could not resolve its placement asks for nothing at all.
///
/// Called by a create BEFORE it takes, or AFTER it has released, this service's store, so no
/// guard is held across the hops. THREE calls, with no caller-sized value in any of them: the
/// group's name and description are built from the directory's own identifier, and the rules
/// authored on it are a fixed set ([`CONTROLLER_INGRESS_RULES`]) scoped to the block the owner
/// of VPCs reports.
///
/// A hop that could not be made is evidence of nothing and gates no create: the directory
/// keeps the placement the request named and simply reports no security group, rather than an
/// id that names no group anywhere.
pub async fn provision_controllers_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    directory_id: &str,
    vpc_id: Option<&str>,
) -> Option<String> {
    let vpc_id = vpc_id?;
    let client = ec2(ctx, region).or_else(|| {
        ::tracing::warn!(
            "the request names no Region label, so no security group can be created for the \
             directory's servers"
        );
        None
    })?;
    let group_id = match client
        .create_security_group()
        .group_name(controllers_group_name(directory_id))
        .description(controllers_group_description(directory_id))
        .vpc_id(vpc_id)
        .send()
        .await
    {
        // The id EC2 minted, read off its own answer: this service reports the group the
        // owner of security groups holds and never a value of its own.
        Ok(output) => output.group_id,
        Err(error) => {
            // At `warn`: a directory whose servers no security group was created for reports
            // none, which is worth seeing, and this IS why.
            ::tracing::warn!(
                %region,
                %vpc_id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the security group for a directory's servers could not be created in EC2"
            );
            None
        }
    }?;
    // The group is what the directory's servers are REACHED through, so provisioning it is
    // not finished until the traffic those servers answer is admitted through it: the rules
    // are authorized in the same owner, against the group it just minted.
    //
    // The id is reported whatever that authorization answers, and deliberately: the group
    // EC2 minted EXISTS, it is the one the teardown has to release again
    // ([`release_controllers_group`]), and a directory that reported none while the owner of
    // security groups held one would leave that group behind for good. A group the rules
    // could not be authorized on is logged at `warn` where it happens.
    authorize_controllers_ingress(&client, region, &group_id, vpc_id).await;
    Some(group_id)
}

/// Author the inbound rules of the group this service provisioned for a directory's servers,
/// each scoped to the block of the VPC those servers sit on.
///
/// A group with no inbound rule admits nothing, so the servers behind it are reachable by
/// nothing — which is why the set is part of provisioning the group rather than something a
/// caller is left to add: the directory would otherwise report a group that says its own
/// servers cannot be reached. The rules are authorized in the service that OWNS security
/// groups, over the same hop the group was minted through, so `DescribeSecurityGroups` on the
/// id this service reports answers with them.
///
/// The SOURCE of every rule is the VPC's own block, as the owner of VPCs reports it
/// ([`super::ec2_subnets::vpc_cidr_block`]): what the directory's servers serve is the
/// network they are placed on, so a group open to every address would admit traffic from
/// outside the VPC the caller put the directory in. A VPC whose block that owner does not
/// report leaves the group as EC2 minted it rather than being given a wider range.
///
/// Called with this service's store RELEASED, so no guard is held across the hops. ONE hop
/// for the block and ONE for the rules, with no caller-sized value in either: the set is
/// fixed ([`CONTROLLER_INGRESS_RULES`]).
async fn authorize_controllers_ingress(
    client: &::aws_sdk_ec2::Client,
    region: &str,
    group_id: &str,
    vpc_id: &str,
) {
    let Some(block) = super::ec2_subnets::vpc_cidr_block(client, region, vpc_id).await else {
        ::tracing::warn!(
            %region,
            %vpc_id,
            "the VPC of a directory's servers reports no block, so the group provisioned for \
             them carries no inbound rule"
        );
        return;
    };
    let permissions: Vec<::aws_sdk_ec2::types::IpPermission> = CONTROLLER_INGRESS_RULES
        .iter()
        .map(|rule| {
            ::aws_sdk_ec2::types::IpPermission::builder()
                .ip_protocol(rule.protocol)
                .from_port(rule.from_port)
                .to_port(rule.to_port)
                .ip_ranges(
                    ::aws_sdk_ec2::types::IpRange::builder()
                        .cidr_ip(block.as_str())
                        .build(),
                )
                .build()
        })
        .collect();
    if let Err(error) = client
        .authorize_security_group_ingress()
        .group_id(group_id)
        .set_ip_permissions(Some(permissions))
        .send()
        .await
    {
        // At `warn`: a directory reporting a group that admits nothing is worth seeing, and
        // this IS why.
        ::tracing::warn!(
            %region,
            %group_id,
            %block,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the inbound rules of the group for a directory's servers could not be \
             authorized in EC2"
        );
    }
}
