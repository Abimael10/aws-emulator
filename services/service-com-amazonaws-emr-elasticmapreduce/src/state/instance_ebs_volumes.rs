// SPDX-License-Identifier: CC0-1.0

//! The Amazon EBS volumes a cluster's NODES hold: which volume of the account sits at which
//! device of one node.
//!
//! One concern of this service's state, and the per-INSTANCE half of what a collection's
//! `EbsConfiguration` asks for: the collection reports the devices it maps onto every one of
//! its instances ("the EBS block devices that are mapped to this instance group",
//! `InstanceGroup$EbsBlockDevices` doc), and each node of it really holds one Amazon EBS
//! volume per mapped device — "the list of Amazon EBS volumes that are attached to this
//! instance" (`Instance$EbsVolumes` doc), each carrying "the device name that is exposed to
//! the instance" and "the volume identifier of the EBS volume".
//!
//! A volume is Amazon EC2's resource, not this service's, exactly as an instance is
//! ([`super::cluster_instances`]). It is created BY THE LAUNCH: Amazon EMR states the devices
//! its collection asks for as the launch's own block device mapping — "the block device
//! mapping, which defines the EBS volumes … to attach to the instance at launch"
//! (`RunInstancesRequest$BlockDeviceMappings` doc, Amazon EC2 model) — so the node holds them
//! from the moment Amazon EC2 launched it, and nothing attaches a volume to a node
//! afterwards. What this service keeps is what Amazon EC2 ANSWERED for that instance, read
//! back as part of that same launch ([`super::launch_cluster_instances`]): the volume at each
//! device, so a `VolumeId` reported here is a volume Amazon EC2 itself answers
//! `DescribeVolumes` for, at the device it really holds it at, and a launch Amazon EC2 created
//! no volume for reports none. Nothing is minted here.

use crate::ops::types::EbsVolume;

/// ONE Amazon EBS volume a cluster's node holds.
///
/// [`InstanceEbsVolume::to_ebs_volume`] projects it onto the modelled `EbsVolume` shape, so
/// `ListInstances` reports volumes that really exist rather than anything built from its own
/// input.
#[derive(Debug, Clone)]
pub struct InstanceEbsVolume {
    /// `EbsVolume$Device`: "the device name that is exposed to the instance, such as
    /// `/dev/sdh`" — the device its collection maps the volume at
    /// ([`super::ebs_device_name`]), as Amazon EC2 reports the attachment.
    pub device: String,
    /// `EbsVolume$VolumeId`: the volume Amazon EC2 minted for the launch, which is what makes
    /// this record a reference to a volume that exists.
    pub volume_id: String,
}

impl InstanceEbsVolume {
    /// The wire `EbsVolume` for this record.
    pub fn to_ebs_volume(&self) -> EbsVolume {
        EbsVolume {
            device: Some(self.device.clone()),
            volume_id: Some(self.volume_id.clone()),
        }
    }
}
