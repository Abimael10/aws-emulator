// SPDX-License-Identifier: CC0-1.0

//! The elastic network interface an `awsvpc` task RUNS ON, as Amazon VPC holds it.
//!
//! `RegisterTaskDefinition$networkMode`: "If the network mode is `awsvpc`, the task is
//! allocated an elastic network interface", and `Attachment$details` "for elastic network
//! interfaces ... includes the network interface ID, the MAC address, the subnet ID, and the
//! private IPv4 address". That interface is an Amazon VPC resource: Amazon ECS creates it in
//! the subnet the launch named, it holds one of that subnet's addresses while the task runs,
//! and it is deleted when the task stops — so the id the attachment reports is an interface
//! Amazon EC2's own `DescribeNetworkInterfaces` returns, and the two views of one interface
//! agree because there is only one record of it.
//!
//! What that record carries follows from the launch: it sits in the launch's subnet with the
//! launch's security groups, it is MANAGED by Amazon ECS rather than by the account
//! ([`ECS_INTERFACE_REQUESTER`], which is what `NetworkInterface$RequesterManaged` reports),
//! and it receives a public IPv4 address exactly when the launch asked for one
//! (`AwsVpcConfiguration$assignPublicIp`: "whether the task's elastic network interface
//! receives a public IP address") — assigned out of Amazon's own pool by the one place a
//! public address of that store comes from, so the address the task is reachable at is the
//! address Amazon EC2 reports for its interface.
//!
//! LOCK ORDER: the pass below takes this store's lock, releases it, takes the Amazon EC2
//! store's, releases that, and then takes this one's again to record what Amazon VPC
//! created — so no two store locks are ever held at once, the same contract
//! [`evaluate_image_pulls`] has.

use super::*;

use ::service_com_amazonaws_ec2_amazonec2::ops::types::NetworkInterfaceAttachment;
use ::service_com_amazonaws_ec2_amazonec2::state as ec2;
use ::service_com_amazonaws_ec2_amazonec2::ComAmazonawsEc2Amazonec2State;

/// `NetworkInterface$RequesterId` of an interface Amazon ECS created to run a task of the
/// account on: the service's own alias, which is also what makes the interface one "being
/// managed by Amazon Web Services" (`NetworkInterface$RequesterManaged`) rather than one the
/// account created for itself.
pub const ECS_INTERFACE_REQUESTER: &str = "amazon-ecs";

/// The device index the interface of an `awsvpc` task is attached at. Index 0 is the primary
/// interface of a host; the interface a task is given is the one beside it.
const TASK_INTERFACE_DEVICE_INDEX: i32 = 1;

/// `AwsVpcConfiguration$assignPublicIp` asking for a public address: "whether the task's
/// elastic network interface receives a public IP address".
pub const ASSIGN_PUBLIC_IP_ENABLED_VALUE: &str = "ENABLED";

/// The block an interface created in a subnet this emulator holds no CIDR block for takes
/// its address out of: the block AWS gives a default VPC (Amazon VPC User Guide, "Default
/// VPCs").
const DEFAULT_VPC_CIDR_BLOCK: &str = "172.31.0.0/16";

/// What ONE task attachment needs created in Amazon VPC: where the interface goes, what
/// governs it, and whether it receives a public address.
///
/// Collected under this store's lock ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::interfaces_awaiting_allocation`])
/// and answered against the Amazon EC2 store under that store's own, so no two locks are
/// held at once.
#[derive(Debug, Clone)]
pub struct PendingInterface {
    /// The task the interface was allocated for.
    pub task_arn: String,
    /// `Attachment$id` of the attachment it stands for.
    pub attachment_id: String,
    /// The subnet the interface is created in, as the launch named it.
    pub subnet_id: String,
    /// The security groups the launch named, which govern the interface.
    pub security_groups: Vec<String>,
    /// Whether the launch asked for a public IPv4 address.
    pub assigns_public_ip: bool,
    /// The Region the task is in, which the interface's DNS names are built for.
    pub region: String,
}

/// The interface Amazon VPC created for one attachment: the three details that belong to the
/// interface ITSELF, read back off the record Amazon EC2 stored.
#[derive(Debug, Clone)]
pub struct AllocatedInterface {
    pub network_interface_id: String,
    pub mac_address: String,
    pub private_ipv4_address: String,
}

/// Create in Amazon VPC the interface every `awsvpc` task this store placed is owed, and
/// delete the ones whose tasks have stopped.
///
/// Called with NO guard of this service's store held, from the pass every operation that
/// places a task takes and the clock pump drives ([`settle_task_launches`]): Amazon ECS
/// creates the task's interface while the task is `PROVISIONING`, before anything is pulled
/// or started, so the identity is in place by the time the attachment reports it
/// ([`AttachmentRecord::report_allocated_interface`]).
///
/// Nothing is created for a task whose subnet Amazon VPC holds no record of, or when that
/// store is full: the attachment then reports the subnet the launch named and none of the
/// three details that belong to an interface, rather than naming an interface that does not
/// exist.
pub fn settle_task_interfaces<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let (pending, released) = {
        let store = state.lock();
        (
            store.interfaces_awaiting_allocation(),
            store.interfaces_awaiting_release(),
        )
    };
    if pending.is_empty() && released.is_empty() {
        return;
    }
    let Some(ec2_state) = ec2::service_state(engine) else {
        return;
    };
    let allocated: Vec<(PendingInterface, AllocatedInterface)> = {
        let mut ec2_store = ec2_state.lock();
        // The interfaces of tasks that have stopped go first, so the addresses they held are
        // back in their subnets before this pass takes one for a task that is starting.
        for network_interface_id in &released {
            ec2_store.remove_network_interface(network_interface_id);
        }
        pending
            .into_iter()
            .filter_map(|wanted| {
                allocate_interface(&mut ec2_store, &wanted)
                    .map(|allocated| (wanted, allocated))
            })
            .collect()
    };
    if allocated.is_empty() && released.is_empty() {
        return;
    }
    let mut store = state.lock();
    for (wanted, interface) in allocated {
        store.record_allocated_interface(&wanted.task_arn, &wanted.attachment_id, interface);
    }
    for network_interface_id in &released {
        store.record_released_interface(network_interface_id);
    }
}

/// Create ONE task's interface in Amazon VPC, and answer what it carries — `None` when it
/// could not be created: a subnet that store holds no record of, or an interface store that
/// is full.
///
/// Everything about the interface comes off the launch and the subnet it names: the subnet's
/// VPC, zone and CIDR block, the launch's security groups (or, for a launch that named none,
/// the VPC's own default group, which is the group such a launch runs under), and the public
/// address the launch asked for. A subnet this emulator holds no CIDR block for is addressed
/// out of the block AWS gives a default VPC instead ([`DEFAULT_VPC_CIDR_BLOCK`]): every
/// subnet AWS holds has a block, so this is a store that was never told one rather than a
/// placement to refuse, and the interface still reports an address it could really hold. The
/// id, the MAC address, the private DNS name and the `in-use` status are Amazon EC2's own to
/// derive, which is why the record is written through that store's one writer seam
/// ([`ComAmazonawsEc2Amazonec2State::insert_network_interface`]) rather than assembled here.
fn allocate_interface(
    ec2_store: &mut ComAmazonawsEc2Amazonec2State,
    wanted: &PendingInterface,
) -> Option<AllocatedInterface> {
    let subnet = ec2_store.subnets.get(&wanted.subnet_id)?;
    let vpc_id = subnet.vpc_id.clone().unwrap_or_default();
    let availability_zone = subnet.availability_zone.clone().unwrap_or_default();
    let availability_zone_id = subnet
        .availability_zone_id
        .clone()
        .or_else(|| ec2::zone_id_for_zone_name(&availability_zone))
        .unwrap_or_default();
    // The block the interface's address comes out of is the subnet's own
    // (`Subnet$cidrBlock`: "the IPv4 CIDR block assigned to the subnet"), and a subnet this
    // emulator holds no block for falls back to the block AWS gives a default VPC (Amazon
    // VPC User Guide, "Default VPCs"): the interface still has to hold an address it could
    // really be reached at.
    let cidr_block = subnet
        .cidr_block
        .clone()
        .unwrap_or_else(|| DEFAULT_VPC_CIDR_BLOCK.to_string());
    if !ec2_store.has_network_interface_capacity() {
        return None;
    }
    // The address the interface holds comes out of its SUBNET, taken from the one pool every
    // address of that store is drawn from, so no other resource of the account can be handed
    // the address this task is reachable at.
    let private_ipv4_address = ec2_store.mint_private_ip_in_cidr(&cidr_block)?;
    let groups = if wanted.security_groups.is_empty() {
        ec2_store
            .default_security_group_of_vpc(&vpc_id)
            .into_iter()
            .collect()
    } else {
        wanted
            .security_groups
            .iter()
            .map(|group| {
                ec2_store.group_identifier_of(Some(vpc_id.as_str()), Some(group.as_str()), None)
            })
            .collect()
    };
    let attachment = NetworkInterfaceAttachment {
        attach_time: Some(ec2::system_time_to_date_time(SystemTime::now())),
        // The attachment id is of the SERVICE-MANAGED kind: no instance of the account holds
        // this interface, the task does, so the id's own form tells a caller reading the
        // inventory that the attachment is not one it may take down.
        attachment_id: Some(ec2_store.next_id(ec2::SERVICE_NETWORK_INTERFACE_ATTACHMENT_ID_PREFIX)),
        // The interface Amazon ECS creates for a task is torn down with that task.
        delete_on_termination: Some(true),
        device_index: Some(TASK_INTERFACE_DEVICE_INDEX),
        network_card_index: Some(0),
        // No instance holds it: the task it serves is what holds it, and Amazon ECS is the
        // owner of that attachment.
        instance_id: None,
        instance_owner_id: Some(ECS_INTERFACE_REQUESTER.to_string()),
        // The interface is in place by the time this is recorded: the attachment Amazon ECS
        // reports is what the clock takes through its own progression
        // ([`INTERFACE_ATTACH_LIFECYCLE`]), and the Amazon VPC record stands for an
        // interface that exists.
        status: Some(ec2::ATTACHMENT_STATUS_ATTACHED.to_string()),
        // ENA Express is a setting of an attachment something asked for; this asks for none.
        ena_srd_specification: None,
        ena_queue_count: None,
    };
    let network_interface_id = ec2_store.insert_network_interface(ec2::NetworkInterfaceSpec {
        subnet_id: wanted.subnet_id.clone(),
        vpc_id,
        availability_zone,
        availability_zone_id,
        region: wanted.region.clone(),
        // AWS reports an empty description for an interface nothing described.
        description: String::new(),
        interface_type: ec2::NETWORK_INTERFACE_TYPE_INTERFACE.to_string(),
        private_ip_addresses: vec![private_ipv4_address.clone()],
        groups,
        attachment: Some(attachment),
        // Amazon ECS created it to run a task of the account, so Amazon ECS manages it.
        requester: Some(ECS_INTERFACE_REQUESTER.to_string()),
        // Nothing here asks for IPv6 space or tags the interface.
        ..Default::default()
    })?;
    // "You can specify ENABLED only when LaunchType in EcsParameters is set to FARGATE"
    // (`AwsVpcConfiguration$assignPublicIp`): a launch that asked for a public address gets
    // one on this interface, out of Amazon's own pool and through the one place such an
    // address is assigned — so `DescribeNetworkInterfaces` reports the very address the task
    // is reachable at, and a launch that asked for none reports no association at all.
    if wanted.assigns_public_ip {
        ec2_store.assign_amazon_public_ipv4(&network_interface_id, &wanted.region);
    }
    let mac_address = ec2_store
        .network_interfaces
        .get(&network_interface_id)
        .and_then(|interface| interface.mac_address.clone())
        .unwrap_or_default();
    Some(AllocatedInterface {
        network_interface_id,
        mac_address,
        private_ipv4_address,
    })
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Every task attachment that is owed an interface in Amazon VPC: one whose task is
    /// still wanted and which has none allocated yet.
    ///
    /// A pass over the task map, which is bounded by [`MAX_TASKS`], so what one call costs
    /// follows what the store HOLDS rather than anything in a request.
    pub fn interfaces_awaiting_allocation(&self) -> Vec<PendingInterface> {
        self.tasks
            .values()
            .flat_map(|task| {
                let configuration = task.awsvpc_configuration.as_ref();
                let region = ::service_common::arn::parse(&task.task_arn)
                    .map(|arn| arn.region.to_string())
                    .unwrap_or_default();
                task.attachments
                    .iter()
                    .filter(|attachment| attachment.is_elastic_network_interface())
                    .filter(|attachment| attachment.allocated.is_none())
                    // An attachment the clock is RELEASING stands for an interface that is
                    // on its way out ([`INTERFACE_DETACH_LIFECYCLE`]), so nothing is owed
                    // one for it.
                    .filter(|attachment| {
                        attachment.status != ATTACHMENT_STATUS_DELETED
                            && !attachment.is_detaching()
                    })
                    .filter_map(move |attachment| {
                        Some(PendingInterface {
                            task_arn: task.task_arn.clone(),
                            attachment_id: attachment.id.clone(),
                            subnet_id: attachment.subnet_id()?.to_string(),
                            security_groups: configuration
                                .and_then(|configuration| configuration.security_groups.clone())
                                .unwrap_or_default(),
                            assigns_public_ip: configuration
                                .and_then(|configuration| configuration.assign_public_ip.as_deref())
                                == Some(ASSIGN_PUBLIC_IP_ENABLED_VALUE),
                            region: region.clone(),
                        })
                    })
                    .collect::<Vec<PendingInterface>>()
            })
            .collect()
    }

    /// Every interface Amazon VPC still holds for an attachment whose task has STOPPED: the
    /// interface Amazon ECS created for a task is torn down with the task, so the address it
    /// held goes back to its subnet and nothing goes on resolving the id the attachment
    /// reported.
    pub fn interfaces_awaiting_release(&self) -> Vec<String> {
        self.tasks
            .values()
            .flat_map(|task| task.attachments.iter())
            .filter(|attachment| attachment.status == ATTACHMENT_STATUS_DELETED)
            .filter(|attachment| !attachment.interface_released)
            .filter_map(|attachment| {
                attachment
                    .allocated
                    .as_ref()
                    .map(|allocated| allocated.network_interface_id.clone())
            })
            .collect()
    }

    /// Record the interface Amazon VPC created for one attachment, and report it on that
    /// attachment when the interface is already in place.
    ///
    /// The identity is what the attachment reports once the interface is attached
    /// ([`AttachmentRecord::report_allocated_interface`]); an attachment the clock has
    /// already taken to [`ATTACHMENT_STATUS_ATTACHED`] reports it from here, so an
    /// attachment that is in place never reports a subnet without the interface in it.
    pub fn record_allocated_interface(
        &mut self,
        task_arn: &str,
        attachment_id: &str,
        interface: AllocatedInterface,
    ) {
        let Some(task) = self.tasks.get_mut(task_arn) else {
            return;
        };
        let Some(attachment) = task
            .attachments
            .iter_mut()
            .find(|attachment| attachment.id == attachment_id)
        else {
            return;
        };
        attachment.allocated = Some(interface);
        if attachment.is_attached() && attachment.report_allocated_interface() {
            task.version += 1;
        }
    }

    /// Record that the interface `network_interface_id` has been deleted in Amazon VPC, so
    /// no later pass asks for it to be deleted again.
    pub fn record_released_interface(&mut self, network_interface_id: &str) {
        for task in self.tasks.values_mut() {
            for attachment in &mut task.attachments {
                if attachment.allocated.as_ref().is_some_and(|allocated| {
                    allocated.network_interface_id == network_interface_id
                }) {
                    attachment.interface_released = true;
                }
            }
        }
    }
}
