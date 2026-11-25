// SPDX-License-Identifier: CC0-1.0

//! Which Amazon EC2 instances are RUNNING on an Outpost.
//!
//! An Outpost's `AssetInstances` is "a list of Amazon EC2 instances, belonging to all
//! accounts, running on the specified Outpost" (`ListAssetInstances`), and its blocking
//! instances are "all running Amazon EC2 instances on the Outpost"
//! (`ListBlockingInstancesForCapacityTask`) — so neither is a fact a caller states about
//! the Outpost: both FOLLOW from where the instances of the account were launched, and no
//! operation of THIS API launches one.
//!
//! Amazon EC2 is the service that owns an instance, and the capacity a caller puts on an
//! Outpost's hardware through it is a Dedicated Host allocated on that hardware
//! (`AllocateHosts` with `OutpostArn`, optionally naming the `AssetIds` to allocate on).
//! EC2 reports the Outpost such a host was allocated on (`Host$OutpostArn`), the hardware
//! asset it sits on (`Host$AssetId`) and "the IDs and instance type that are currently
//! running on the Dedicated Host" with the account that owns each (`Host$Instances`) —
//! every member this service reports an instance by. So this service mints no instance of
//! its own: it reads EC2's own answer and projects it, which is why an instance launched
//! through EC2 appears here at once, and one that has stopped, terminated or had its host
//! released is gone from here at once too.
//!
//! The refresh runs at the ONE seam every operation of this service reaches the store
//! through ([`super::service_state`]), so no report of an Outpost can be answered from a
//! stale projection. It is a pure recompute of a derived member — the arrangement EC2's own
//! `Host$Instances` has — and moves no state of its own.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every
//! other primitive there.

use super::*;

/// The service that owns an Amazon EC2 instance, and the Dedicated Hosts an account's
/// Outpost capacity is allocated as.
use ::service_com_amazonaws_ec2_amazonec2 as ec2;

/// The Amazon EC2 instances EC2 holds on each Outpost, keyed by the `OutpostArn` of the
/// Outpost the Dedicated Host they run on was allocated on.
///
/// In Dedicated Host id order, and within one host in instance-id order — so two reads of
/// one Outpost report the same instances in the same order, and a page boundary of either
/// list over them names the same instance on every read. (EC2's own `Host$Instances` is in
/// no order of its own: it is recomputed from an instance store that is a hash map.)
pub type Ec2InstancesByOutpost = BTreeMap<String, Vec<AssetInstance>>;

/// The `AssetInstance` projection of one instance EC2 reports on `host`.
///
/// Every member is read off EC2's own answer and nothing is invented: the instance id and
/// instance type the host reports for it (`HostInstance$InstanceId` / `$InstanceType`), "the
/// ID of the Amazon Web Services account that owns the instance" (`$OwnerId`), and the
/// Outpost hardware asset EC2 allocated the host on (`Host$AssetId`), which is absent for a
/// host allocated on an Outpost without naming one.
///
/// `AwsServiceName` is "the Amazon Web Services service name of the instance"
/// (`AssetInstance$AwsServiceName`) — the Amazon Web Services service that owns the
/// capacity, one of the modelled [`crate::ops::types::A_W_S_SERVICE_NAME_VALUES`] — so an
/// instance a customer launched itself carries none, which is what a host of the account
/// holds.
fn projection(
    host: &ec2::ops::types::Host,
    instance: &ec2::ops::types::HostInstance,
) -> AssetInstance {
    AssetInstance {
        instance_id: instance.instance_id.clone(),
        instance_type: instance.instance_type.clone(),
        asset_id: host.asset_id.clone(),
        account_id: instance.owner_id.clone(),
        aws_service_name: None,
    }
}

/// What Amazon EC2 holds running on the Outposts of `store`, as this service reports an
/// instance.
///
/// A host allocated on no Outpost (`Host$OutpostArn` absent) is capacity in the Region
/// rather than on an Outpost, so it contributes nothing here.
fn instances_by_outpost(store: &ec2::ComAmazonawsEc2Amazonec2State) -> Ec2InstancesByOutpost {
    let mut by_outpost = Ec2InstancesByOutpost::new();
    for host in store.dedicated_hosts.values() {
        let Some(outpost_arn) = host.outpost_arn.clone() else {
            continue;
        };
        let mut running: Vec<AssetInstance> = host
            .instances
            .iter()
            .flatten()
            .map(|instance| projection(host, instance))
            .collect();
        // One host's instances, in one stable order: see [`Ec2InstancesByOutpost`].
        running.sort_by(|one, other| one.instance_id.cmp(&other.instance_id));
        by_outpost.entry(outpost_arn).or_default().extend(running);
    }
    by_outpost
}

impl ComAmazonawsOutpostsOutpostsolafserviceState {
    /// Record `by_outpost` — what Amazon EC2 holds running on each Outpost — on the
    /// Outposts of this store.
    ///
    /// Each Outpost takes the instances EC2 holds on the Dedicated Hosts allocated on the
    /// Outpost its own ARN names, and nothing else: an Outpost EC2 holds no host of
    /// reports no instance, exactly as one nobody has launched an instance on does.
    ///
    /// The asset an instance is reported on is the one EC2 allocated its host on, and it is
    /// reported only when this Outpost HOLDS hardware by that id
    /// ([`OutpostRecord::assets`]): an asset id no unit of this Outpost carries names none
    /// of its hardware, so the instance is reported on no asset rather than on one no
    /// `ListAssets` of this Outpost could answer for.
    pub fn record_ec2_instances(&mut self, by_outpost: &Ec2InstancesByOutpost) {
        for record in self.outposts.values_mut() {
            let held: BTreeSet<&str> = record
                .assets
                .iter()
                .filter_map(|asset| asset.asset_id.as_deref())
                .collect();
            let mut running: Vec<AssetInstance> = record
                .outpost
                .outpost_arn
                .as_deref()
                .and_then(|outpost_arn| by_outpost.get(outpost_arn))
                .cloned()
                .unwrap_or_default();
            for instance in &mut running {
                if instance
                    .asset_id
                    .as_deref()
                    .is_some_and(|asset_id| !held.contains(asset_id))
                {
                    instance.asset_id = None;
                }
            }
            record.ec2_instances = running;
        }
    }
}

/// Refresh every Outpost's Amazon EC2 instances in `outposts` from the Amazon EC2 store of
/// the shared `engine` — or leave them as they are where this emulator serves no Amazon EC2
/// at all, which holds no instance to report.
///
/// Called by [`super::service_state`], so every operation of this service answers from what
/// EC2 holds at the instant it is served. The two stores are locked one after the other and
/// never together: EC2's answer is read out and its guard dropped before this service's
/// store is written, so no call of this service can hold one store while waiting for the
/// other.
///
/// Reading EC2's answer materialises the transitions ITS clock already owes — among them
/// the move that takes a stopping or terminating instance off its host, owed at the same
/// instant whether or not anybody had called EC2 — exactly as any read of EC2 itself does,
/// and recomputes the hosts' own derived instance list from its instance store. Nothing
/// else of EC2's state is touched.
pub fn sync_ec2_outpost_instances<Eng: Send + Sync + 'static>(
    engine: &Eng,
    outposts: &::engine::ServiceState<ComAmazonawsOutpostsOutpostsolafserviceState>,
) {
    let Some(ec2_state) = ec2::state::service_state(engine) else {
        return;
    };
    let by_outpost = {
        let mut ec2_store = ec2_state.lock();
        ec2_store.materialize_due_transitions();
        ec2_store.resync_dedicated_host_instances();
        instances_by_outpost(&ec2_store)
    };
    outposts.lock().record_ec2_instances(&by_outpost);
}
