// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a pod of a cluster holds, which is an address of the VPC the
//! cluster's nodes run in.
//!
//! A pod of an Amazon EKS cluster is addressed by the cluster's networking plugin,
//! and that plugin gives a pod an address OF THE SUBNET its node sits in: the
//! address is taken out of that subnet's IPv4 CIDR block and recorded as a
//! SECONDARY private address on the node's own elastic network interface, which is
//! why a client that reads the pod's `status.podIP` finds that same address among
//! the `PrivateIpAddresses` Amazon EC2 reports for that interface. Nothing about
//! the address is a value this service invents: the interface, the subnet and the
//! pool the address comes out of are Amazon EC2's records
//! ([`ec2::ComAmazonawsEc2Amazonec2State::mint_private_ip_in_cidr`] and
//! [`ec2::ComAmazonawsEc2Amazonec2State::assign_secondary_private_ips`], the same
//! two writers `AssignPrivateIpAddresses` goes through), so no other resource of
//! the account can be handed the address a pod is reachable at.
//!
//! A pod the cluster has placed on no node is addressed by nothing and reports no
//! address, and a pod that LEAVES its node gives its address back to the subnet —
//! the plugin releases what it took, so an address is held for exactly as long as
//! the pod that holds it.
//!
//! The addresses of a cluster whose NODES are gone need no release here: a node is
//! an Amazon EC2 instance, and the instance's interface — with every address
//! recorded on it — is deleted by Amazon EC2 when the instance is terminated, which
//! is what releasing a node group's capacity does.
//!
//! LOCK ORDER: the pass below takes this service's store, releases it, takes the
//! Amazon EC2 store's, releases that, and then takes this one's again to record what
//! the plugin was given — so no two store locks are ever held at once, the same
//! contract [`super::kubernetes_image_pull::settle_due_pulls`] has.

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

use super::ComAmazonawsEksAwswesleyfrontendState as ServiceStore;

/// The address one pod holds, and the node interface it was taken on.
///
/// Recorded on the pod ([`super::kubernetes_objects::PodObject::address`]), so
/// every read of that pod reports the same address, and the release that gives it
/// back names the very interface it was assigned to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodAddress {
    /// The private IPv4 address itself, which is what `Pod$status.podIP` reports.
    pub address: String,
    /// The node's elastic network interface the address is a secondary address of.
    pub network_interface_id: String,
}

/// How many pod addresses one request to a cluster's API server settles.
///
/// CHOSEN, not published: the pods a cluster owes an address accumulate while
/// nobody calls — a cluster holds up to
/// [`super::kubernetes_objects::MAX_PODS_PER_NAMESPACE`] pods per namespace — and
/// each one asks the Amazon EC2 store for an address of its node's subnet, so one
/// request must not drain an arbitrary backlog of them. The rest stay due and are
/// settled by the next request, which is what a plugin working through the pods of
/// its node does anyway: nothing is lost, and no address changes, because an
/// address once taken is recorded on the pod.
const MAX_ADDRESSES_PER_REQUEST: usize = 16;

/// The Amazon EC2 records a cluster's networking plugin takes its pod addresses
/// out of.
///
/// Taken once, when the cluster's API address is opened, and held for as long as
/// that address is — the way the registries a kubelet pulls from are
/// ([`super::kubernetes_image_pull::PullRegistries`]): a pod is addressed when the
/// cluster places it on a node, which is an instant no call of this service is
/// holding the engine at.
#[derive(Clone)]
pub struct PodAddressPlane {
    /// The store the account's network interfaces, subnets and addresses live in;
    /// `None` for an emulator with no Amazon EC2 mounted, which holds no interface
    /// for any node and therefore no address for any pod.
    ec2: Option<::engine::ServiceState<ec2::ComAmazonawsEc2Amazonec2State>>,
}

/// Written by hand rather than derived: a store handle does not render, and what is
/// diagnosable is whether the records are reachable at all.
impl ::std::fmt::Debug for PodAddressPlane {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("PodAddressPlane")
            .field("ec2", &self.ec2.is_some())
            .finish()
    }
}

impl PodAddressPlane {
    /// The Amazon EC2 records in `engine`.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Self {
        Self {
            ec2: ec2::service_state(engine),
        }
    }
}

/// One pod of a cluster that is owed an address: which pod, and the instance its
/// node is.
#[derive(Debug)]
struct DueAddress {
    namespace: String,
    pod_name: String,
    /// The pod's own uid, so the address is recorded on the very pod it was taken
    /// for and not on a later pod of the same name.
    pod_uid: String,
    /// The Amazon EC2 instance the node is, whose interface the address is taken
    /// on.
    instance_id: String,
}

/// Give every pod of the cluster keyed `key` that the cluster has placed on a node
/// the address its node's networking plugin owes it, and give back the addresses of
/// the pods that have left their node.
///
/// Called with NO store of this service held (see the module docs), and idempotent:
/// a pod that already holds an address is left exactly as it is, so running this
/// again — which every later request does — changes nothing a caller sees.
pub fn settle_due_pod_addresses(
    state: &::engine::ServiceState<ServiceStore>,
    plane: &PodAddressPlane,
    key: &(String, String),
) {
    // An emulator with no Amazon EC2 has no interface to take an address on, and
    // no pod of it can be holding one — so nothing is taken and nothing is given
    // back, rather than a cluster's records being drained against a store that
    // cannot answer for them.
    let Some(ec2_state) = plane.ec2.as_ref() else {
        return;
    };
    let (due, released) = {
        let mut store = state.lock();
        let nodes = store.scheduler_nodes(&key.0, &key.1);
        let Some(plane) = store.kubernetes_api.get_mut(key) else {
            return;
        };
        let released = ::std::mem::take(&mut plane.objects.released_addresses);
        let mut due: Vec<DueAddress> = Vec::new();
        'walk: for (namespace, held) in &plane.objects.namespaces {
            for (pod_name, pod) in &held.pods {
                if due.len() >= MAX_ADDRESSES_PER_REQUEST {
                    break 'walk;
                }
                if pod.address.is_some() {
                    continue;
                }
                // A pod the cluster has placed on NO node is addressed by nothing:
                // there is no node interface for its address to be a secondary
                // address of.
                let Some(node) = pod.node_name.as_deref() else {
                    continue;
                };
                let Some(instance_id) = nodes
                    .iter()
                    .find(|held| held.name == node)
                    .map(|held| held.instance_id.clone())
                    .filter(|instance_id| !instance_id.is_empty())
                else {
                    continue;
                };
                due.push(DueAddress {
                    namespace: namespace.clone(),
                    pod_name: pod_name.clone(),
                    pod_uid: pod.uid.clone(),
                    instance_id,
                });
            }
        }
        (due, released)
    };
    if due.is_empty() && released.is_empty() {
        return;
    }
    let taken: Vec<(DueAddress, PodAddress)> = {
        let mut ec2_store = ec2_state.lock();
        // The addresses of pods that are gone go back FIRST, so the subnet is
        // holding them again before this pass takes one for a pod that is starting.
        for address in &released {
            ec2_store
                .release_secondary_private_ip(&address.network_interface_id, &address.address);
        }
        due.into_iter()
            .filter_map(|wanted| {
                take_node_address(&mut ec2_store, &wanted.instance_id)
                    .map(|address| (wanted, address))
            })
            .collect()
    };
    if taken.is_empty() {
        return;
    }
    let mut store = state.lock();
    let Some(plane) = store.kubernetes_api.get_mut(key) else {
        return;
    };
    for (wanted, address) in taken {
        let Some(pod) = plane
            .objects
            .namespaces
            .get_mut(&wanted.namespace)
            .and_then(|namespace| namespace.pods.get_mut(&wanted.pod_name))
        else {
            // The pod was removed while this pass was asking Amazon EC2 for its
            // address: the address goes back rather than being held by nothing.
            plane.objects.released_addresses.push(address);
            continue;
        };
        if pod.uid != wanted.pod_uid || pod.address.is_some() {
            plane.objects.released_addresses.push(address);
            continue;
        }
        pod.address = Some(address);
    }
}

/// Take one address of the subnet the primary interface of the instance
/// `instance_id` sits in, and record it on that interface as a secondary private
/// address — the assignment a cluster's networking plugin makes for a pod it
/// places on that node.
///
/// `None` — and nothing taken — when the instance holds no attached interface this
/// store knows, when the store holds no CIDR block for its subnet, when the
/// interface already holds as many private addresses as one may hold
/// ([`ec2::MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE`], the ceiling
/// `AssignPrivateIpAddresses` itself refuses past), or when the subnet has no free
/// address left: the pod then reports no address rather than one nothing holds.
fn take_node_address(
    ec2_store: &mut ec2::ComAmazonawsEc2Amazonec2State,
    instance_id: &str,
) -> Option<PodAddress> {
    // Which interface the node's traffic is on is Amazon EC2's own question, and it
    // answers it in one place ([`ec2::ComAmazonawsEc2Amazonec2State::primary_network_interface_of_instance`]).
    let interface = ec2_store.primary_network_interface_of_instance(instance_id)?;
    let network_interface_id = interface.network_interface_id.clone()?;
    let subnet_id = interface.subnet_id.clone()?;
    let held = interface.private_ip_addresses.iter().flatten().count();
    if held >= ec2::MAX_PRIVATE_IPS_PER_NETWORK_INTERFACE {
        return None;
    }
    let cidr_block = ec2_store.subnets.get(&subnet_id)?.cidr_block.clone()?;
    let address = ec2_store.mint_private_ip_in_cidr(&cidr_block)?;
    if !ec2_store
        .assign_secondary_private_ips(&network_interface_id, ::std::slice::from_ref(&address))
    {
        // The interface was read a moment ago under this same lock, so this is the
        // store losing it between two reads: the address goes back to the pool.
        ec2_store.release_private_ips(&[address]);
        return None;
    }
    Some(PodAddress {
        address,
        network_interface_id,
    })
}
